/*
 * NFC 検証用シム実装。
 *
 * I2C バスは i2c_new_master_bus (IDF>=5.2、driver/i2c_master.h) で自前で立て、
 * UnitUnified::add(unit, i2c_master_bus_handle_t) に渡す — port+pins 直渡しの
 * add() オーバーロードは Arduino 専用で ESP-IDF native には無い。M5Unified 依存の
 * wiring ヘルパ (m5_unit_unified_wiring.hpp) も使わない (board-aware プリセット
 * 向けで、任意 GPIO を渡すこの用途には不要かつ M5Unified を引き込んでしまう)。
 */
#include "nfc_shim.h"

#include <M5UnitUnified.hpp>
#include <M5UnitUnifiedNFC.h>
#include <M5Utility.hpp>

#include <driver/i2c_master.h>
#include <esp_log.h>

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <memory>

namespace {
constexpr const char* kTag = "nfc_shim";
}  // namespace

namespace {

m5::unit::UnitUnified g_units;
m5::unit::UnitNFC g_unit{};
bool g_ready = false;
// NFCLayerF/A/B は動作確認済みの診断コード (atom_echo_nfc_test_B) と同じく
// 起動時に1回だけ構築する (2026-07-20, issue #96 切り分け中: 従来は検出試行の
// たびに新規構築していた。デストラクタは=defaultでレジスタは触らないと確認
// 済みだが、まだテストしていない差分としてオブジェクト寿命を揃えてみる)
std::unique_ptr<m5::nfc::NFCLayerF> g_nfc_f;
std::unique_ptr<m5::nfc::NFCLayerA> g_nfc_a;
std::unique_ptr<m5::nfc::NFCLayerB> g_nfc_b;
// 直近に configureNFCMode() で実際にチップへ反映したモード。呼ぶたびに
// CMD_STOP_ALL_ACTIVITIES + nfc_initial_field_on() (RFフィールドの一旦停止/
// 再始動) が走るため、モードが変わらない限り呼び直さない (2026-07-20,
// issue #96: F/A/B を毎回強制再設定していたのが検出率低下の一因と判明。
// 動作確認済みの診断コードは起動時に1回設定するだけでフィールドを継続させていた)
m5::nfc::NFC g_configured_mode = m5::nfc::NFC::None;

void ensure_mode(m5::nfc::NFC mode)
{
    if (g_configured_mode == mode) {
        return;
    }
    auto cfg = g_unit.config();
    cfg.mode = mode;
    g_unit.config(cfg);
    g_unit.configureNFCMode(mode);
    g_configured_mode = mode;
}

// 失敗セッション後の復旧 (2026-07-21 実機で確認): ATTRIB や APDU が FWT 内
// 無応答で死ぬと、カード側は READY/ACTIVE 状態のまま残り、以後の WUPB を
// 全て無視して延々 rc=-2 になる (カードを一度フィールドから外すまで復帰
// しない)。configureNFCMode() は CMD_STOP_ALL_ACTIVITIES + フィールド再始動
// を行うため、カードを電源リセットして IDLE に戻せる。次の呼び出しまでは
// ポーリング間隔 (≥200ms) が空くので、カードの再起動時間は十分足りる
void reset_rf_field()
{
    g_unit.configureNFCMode(m5::nfc::NFC::B);
    g_configured_mode = m5::nfc::NFC::B;
}

// EF 2F01 (共通データ要素) の READ BINARY レスポンス内オフセット。
// plan/nfc-card-identity.md 記載 (ippoan/AlcoholChecker の NfcReader.kt 実装に準拠):
//   offset 10〜: 交付日 (BCD 4バイト = "YYYYMMDD")
//   offset 18〜: 有効期限 (BCD 4バイト)
// ただし同 doc が例示する READ BINARY の Le=0x11(17) では offset18 まで届かないため
// (17バイトだと index 0-16 まで)、当初は Le=0x20(32) の一発読みにしていたが、
// 実カードは Le=0x20 だと完全無応答 (FWT=1.2s 待ってもフレーム無し) になることを
// 実機で確認 (2026-07-21)。NfcReader.kt 実績の Le=0x11 を第一読みに使い、
// 期限は offset 0x11 からのオフセット指定第二読み (Le=0x0B) で取得する
constexpr uint8_t kApduSelectMf[]    = {0x00, 0xA4, 0x00, 0x00};
constexpr uint8_t kApduSelectEf2F01[] = {0x00, 0xA4, 0x02, 0x0C, 0x02, 0x2F, 0x01};
constexpr uint8_t kApduReadBinary1[]  = {0x00, 0xB0, 0x00, 0x00, 0x11};
// EF 2F01 実レイアウト (2026-07-21 実機 hex ダンプで確定、全17バイト):
//   45 0b | 30 30 38 ("008") | 20 23 06 09 (交付日BCD) | 20 28 05 13 (期限BCD) | 46 02 | ff 04
// つまり TLV(tag45, len 0x0B) の中に version 3バイト + 交付日4バイト + 期限4バイト。
// plan/nfc-card-identity.md の offset 10/18 想定は誤りだった (offset 0x11 の
// READ BINARY は SW 6B00 = EF 長 17 バイト確定)
constexpr int kIssueDateOffset  = 5;
constexpr int kExpiryDateOffset = 9;
constexpr int kBcdFieldLen      = 4;  // 4 bytes BCD -> 8 digits (YYYYMMDD)

void bcd4_to_yyyymmdd(const uint8_t* bcd, char* out /* >= 9 bytes */)
{
    for (int i = 0; i < kBcdFieldLen; ++i) {
        out[i * 2]     = static_cast<char>('0' + ((bcd[i] >> 4) & 0x0F));
        out[i * 2 + 1] = static_cast<char>('0' + (bcd[i] & 0x0F));
    }
    out[8] = '\0';
}

bool sw_ok(const uint8_t* rx, uint16_t rx_len)
{
    // rx の末尾2バイトが SW1SW2。APDU 応答が最低2バイト無ければ失敗扱い。
    return rx_len >= 2 && m5::nfc::apdu::is_response_OK(rx[rx_len - 2], rx[rx_len - 1]);
}

}  // namespace

extern "C" int nfc_shim_init(int i2c_port, int sda_gpio, int scl_gpio)
{
    // 起動時から B で begin() する (issue #96: 当面の主用途が免許証読み取りのため。
    // F/A を使うときは ensure_mode() がモード遷移を行う)
    auto cfg  = g_unit.config();
    cfg.mode  = m5::nfc::NFC::B;
    g_unit.config(cfg);

    // UnitUnified::add() は ESP-IDF native では i2c_master_bus_handle_t を取る
    // オーバーロードのみ (port+pins 直渡しは Arduino 専用)。IDF>=5.2 の新 I2C
    // master ドライバでバスを自前で立てて渡す
    i2c_master_bus_config_t bus_cfg{};
    bus_cfg.i2c_port                     = static_cast<i2c_port_t>(i2c_port);
    bus_cfg.sda_io_num                   = static_cast<gpio_num_t>(sda_gpio);
    bus_cfg.scl_io_num                   = static_cast<gpio_num_t>(scl_gpio);
    bus_cfg.clk_source                   = I2C_CLK_SRC_DEFAULT;
    bus_cfg.glitch_ignore_cnt            = 7;
    bus_cfg.flags.enable_internal_pullup = true;

    i2c_master_bus_handle_t bus{};
    if (i2c_new_master_bus(&bus_cfg, &bus) != ESP_OK) {
        return -1;
    }
    if (!g_units.add(g_unit, bus)) {
        return -2;
    }
    if (!g_units.begin()) {
        return -3;
    }
    g_configured_mode = m5::nfc::NFC::B;  // begin() が cfg.mode(=B) を反映済み
    g_nfc_f = std::make_unique<m5::nfc::NFCLayerF>(g_unit);
    g_nfc_a = std::make_unique<m5::nfc::NFCLayerA>(g_unit);
    g_nfc_b = std::make_unique<m5::nfc::NFCLayerB>(g_unit);
    g_ready = true;
    return 0;
}

extern "C" int nfc_shim_poll_felica_idm(char* out_hex, int out_cap)
{
    if (!g_ready || out_cap < 17) {
        return -1;
    }
    // UnitUnified::update() を呼ばずに detect() だけ叩くと常に未検出になる実機
    // 事象を確認 (2026-07-20)。M5 公式サンプル (examples/UnitUnified/NFCF/Detect)
    // も loop() 毎回 Units.update() を呼んでおり、内部状態機械の駆動に必須
    g_units.update();
    ensure_mode(m5::nfc::NFC::F);
    m5::nfc::f::PICC picc{};
    // timeout_ms: detect(PICC&) の既定は100msだが、動作確認済みのdetect(vector&)
    // の既定1000msに揃える (issue #96: 短いタイムアウトが検出漏れの原因と判明)
    if (!g_nfc_f->detect(picc, /*timeout_ms=*/1000U)) {
        return 0;  // 未検出 (エラーではない)
    }
    const std::string idm = picc.idmAsString();
    const int len          = static_cast<int>(idm.size());
    if (len <= 0 || len >= out_cap) {
        return -2;
    }
    std::memcpy(out_hex, idm.c_str(), static_cast<size_t>(len) + 1);
    return len;
}

// NFC-A (Type-A, NTAG21x/MIFARE 等) の UID 検出。既知良品の NTAG213 で
// F/B 検出が無反応な事象の切り分け用に追加 (2026-07-20)
extern "C" int nfc_shim_poll_nfca_uid(char* out_hex, int out_cap)
{
    if (!g_ready || out_cap < 21) {
        return -1;
    }
    g_units.update();
    ensure_mode(m5::nfc::NFC::A);
    m5::nfc::a::PICC picc{};
    // 理由は nfc_shim_poll_felica_idm のコメント参照
    if (!g_nfc_a->detect(picc, /*timeout_ms=*/1000U)) {
        return 0;  // 未検出 (エラーではない)
    }
    const std::string uid = picc.uidAsString();
    const int len          = static_cast<int>(uid.size());
    if (len <= 0 || len >= out_cap) {
        return -2;
    }
    std::memcpy(out_hex, uid.c_str(), static_cast<size_t>(len) + 1);
    return len;
}

extern "C" int nfc_shim_read_license_expiry(char* out_issue, int issue_cap, char* out_expiry, int expiry_cap)
{
    if (!g_ready || issue_cap < 9 || expiry_cap < 9) {
        return -1;
    }
    g_units.update();  // 理由は nfc_shim_poll_felica_idm のコメント参照
    ensure_mode(m5::nfc::NFC::B);

    // issue #96 続報 (2026-07-21 実機ログで判明): M5_LOG_LEVEL=4 で観測した結果、
    // detect() 自体は本番経路でも成功していた (PUPI ログあり)。ところが detect()
    // は検出直後に内部で HLTB を送ってカードを HALT に落とすため、続く select()
    // 内の WUPB (HALT からの起床) に免許証が一度も応答せず rc=-3 になっていた
    // (wakeup 成功時に出るはずの ATQB protocol ログが皆無)。
    // そこで detect()+select() をやめ、select() (WUPB→ATTRIB) だけを予算内で回す。
    // WUPB は IDLE のカードも起こすので存在検出を兼ねられ、HLTB を送らないため
    // HALT 復帰問題そのものを回避できる
    m5::nfc::b::PICC picc{};
    bool selected         = false;
    const auto timeout_at = m5::utility::millis() + 1000U;
    do {
        // ATTRIB の応答にも ATQB の FWI (実測 FWI=12 → FWT≈1.2s) が適用されるが、
        // select() の既定タイムアウトは TIMEOUT_ATTRIB=50ms しかなく、実機で
        // 「ATQB 成功 → 54ms 後に Failed to select: rx_len=0」の取りこぼしを
        // 確認 (2026-07-21)。FWT 相当まで待つ
        if (g_nfc_b->select(picc, /*timeout_ms=*/1300U)) {
            selected = true;
            break;
        }
    } while (m5::utility::millis() <= timeout_at);
    if (!selected) {
        // ATTRIB 途中失敗でカードが READY のまま固まっている可能性もあるので、
        // カード無しの定常経路でも毎回フィールドを再始動して次回に備える
        // (フィールド再始動はカード不在時は実質無害。次の WUPB まで ≥200ms)
        reset_rf_field();
        return -2;  // カード無し (WUPB 無応答) または ATTRIB 失敗 (ログで区別可)
    }

    // issue #96 続報 (2026-07-21 実機で判明): ATTRIB 後の APDU は ISO-DEP
    // (I-block, PCB 0x02/0x03) フレーミングが必須。生 APDU を transceive すると
    // 先頭バイト CLA=0x00 が PCB として解釈不能でカードが無視し、全てタイム
    // アウトしていた (rc=-4)。さらに ATQB の FWI=12 → FWT≈1.2s のため既定の
    // fwt_ms=100 では不足。NFCBFileSystem の構築副作用で activatedPICC の
    // FWI/FSC から isoDEP config を正しく設定し、APDU 自体は AlcoholChecker
    // (Android, NfcReader.kt) で実績のあるバイト列を transceiveAPDU で送る
    m5::nfc::NFCBFileSystem fs_cfg(*g_nfc_b);
    (void)fs_cfg;
    auto* dep = g_nfc_b->isoDEP();

    uint8_t rx[64];
    uint16_t rx_len;

    rx_len = sizeof(rx);
    if (!dep->transceiveAPDU(rx, rx_len, kApduSelectMf, sizeof(kApduSelectMf)) || !sw_ok(rx, rx_len)) {
        g_nfc_b->deactivate();
        reset_rf_field();  // 途中死したカードを IDLE に戻す
        return -4;  // SELECT MF 失敗 (免許証以外の Type-B カードの可能性)
    }

    rx_len = sizeof(rx);
    if (!dep->transceiveAPDU(rx, rx_len, kApduSelectEf2F01, sizeof(kApduSelectEf2F01)) || !sw_ok(rx, rx_len)) {
        g_nfc_b->deactivate();
        reset_rf_field();  // 途中死したカードを IDLE に戻す
        return -5;  // SELECT EF 2F01 失敗
    }

    // EF は全17バイトなので Le=0x11 の一発読みで交付日・期限とも取れる
    // (Le=0x20 だと実カードが不安定になる事象を実機で確認済み)
    rx_len = sizeof(rx);
    if (!dep->transceiveAPDU(rx, rx_len, kApduReadBinary1, sizeof(kApduReadBinary1)) || !sw_ok(rx, rx_len)) {
        g_nfc_b->deactivate();
        reset_rf_field();  // 途中死したカードを IDLE に戻す
        return -6;  // READ BINARY 失敗
    }
    g_nfc_b->deactivate();

    const int data_len = static_cast<int>(rx_len) - 2;
    if (data_len < kExpiryDateOffset + kBcdFieldLen) {
        return -7;  // 想定より短い (EF 実長 17 バイトの想定と不一致)
    }

    bcd4_to_yyyymmdd(rx + kIssueDateOffset, out_issue);
    bcd4_to_yyyymmdd(rx + kExpiryDateOffset, out_expiry);
    return 0;
}
