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
// 全て無視して延々 rc=-2 になる (カードを一度フィールドから外すまで復帰しない)。
// 復旧はカード側とチップ側の両方が必要 (どちらか片方では直らないことを実機で確認):
//  1. 本物のフィールド断 (OP レジスタの tx_en/rx_en を直接落とし ≥5ms 待つ) で
//     カードを電源リセットして IDLE へ。configureNFCMode() 単体ではフィールドは
//     切れない (nfc_initial_field_on が "Already tx_en" で false を返すだけ)
//  2. フィールド OFF の状態で configureNFCMode(B) を呼び、STOP_ALL_ACTIVITIES +
//     全レジスタ再設定で失敗セッションのチップ状態 (NRT/FIFO 等) をクリアする。
//     フィールドが落ちているので今度は nfc_initial_field_on の正規経路
//     (CMD_NFC_INITIAL_FIELD_ON → tx_en|rx_en) が成功しフィールドも復帰する
// 呼び出し側は再設定後 100ms 以上空けてから WUPB を再開すること (再設定直後
// ~20ms での WUPB は全滅する事象を実機確認。旧実装が偶然動いていたのは
// ポーリング間隔 200ms が空いていたため)
// RF レギュレータ電圧を最大化する (2026-07-21 実験): configure_nfc_b() は
// reg 0x2C に 0xD0 (reg_s=1 手動・中間値) を書くが、Grove 5V 供給なので
// 上限 0xF8 (rege=1111, 目標 5.1V、実際は供給-ドロップアウトでクランプ) まで
// 上げてフィールド振幅=結合マージンを稼ぐ。TX ドライバ抵抗 (d_res) は既に
// 最小=最大出力のため、レジスタで上げられる残りはここだけ
void boost_rf_power()
{
    g_unit.writeRegulatorVoltageControl(0xF8);
}

void reset_rf_field()
{
    uint8_t op = 0;
    if (g_unit.readOperationControl(op)) {
        const auto rf_bits =
            static_cast<uint8_t>(m5::unit::st25r3916::regval::tx_en | m5::unit::st25r3916::regval::rx_en);
        g_unit.writeOperationControl(static_cast<uint8_t>(op & static_cast<uint8_t>(~rf_bits)));
        m5::utility::delay(10);
    }
    g_unit.configureNFCMode(m5::nfc::NFC::B);  // 0x2C が 0xD0 に戻るので直後に boost し直す
    g_configured_mode = m5::nfc::NFC::B;
    boost_rf_power();
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
    boost_rf_power();
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

namespace {

// 1 セッション試行: select (WUPB→ATTRIB) → SELECT MF → SELECT EF 2F01 → READ BINARY。
// issue #96 で確定した要点:
//  - detect() は使わない (検出直後の HLTB でカードが HALT に落ち、免許証は WUPB
//    起床に応答しない)。select() の WUPB は IDLE も起こすので存在検出を兼ねる
//  - ATTRIB 応答には ATQB の FWI (実測 FWI=12 → FWT≈1.24s) が適用されるため
//    select() の既定 TIMEOUT_ATTRIB=50ms では取りこぼす → 1300ms
//  - ATTRIB 後の APDU は ISO-DEP (I-block) フレーミング必須。NFCBFileSystem の
//    構築副作用で activatedPICC の FWI/FSC から isoDEP config を設定し、
//    AlcoholChecker (NfcReader.kt) 実績のバイト列を transceiveAPDU で送る
int try_read_license_once(char* out_issue, char* out_expiry)
{
    m5::nfc::b::PICC picc{};
    const auto t0 = m5::utility::millis();
    // ATTRIB 待ちは 100ms (2026-07-21 短縮): 応答するカードは数十 ms で返す一方、
    // 応答しないケースは FWT (FWI=12 → 1.24s) まで待っても来ないことを実測済み。
    // 早く見切って即リセット→再試行した方がトータルの検出が速い
    if (!g_nfc_b->select(picc, /*timeout_ms=*/50U)) {
        // 失敗の種別を所要時間で判別する (2026-07-21、静止カードが無反応になる
        // 問題の対策): WUPB 無応答なら req timeout 5ms 前後で即戻るが、ATQB を
        // 受信して ATTRIB 応答待ち (FWT≈1.3s) で死んだ場合は長くかかる。後者の
        // カードは READY 状態でスタックし以後 WUPB を無視するため (ISO 14443-3:
        // READY は REQB/WUPB に応答しない)、-3 を返して呼び出し元に即リセット
        // させる。カードを動かすと反応するのはフィールド出入りの再起動で
        // IDLE に戻るため — 静止時は明示リセットが必須
        return (m5::utility::millis() - t0 >= 30U) ? -3 : -2;
    }

    m5::nfc::NFCBFileSystem fs_cfg(*g_nfc_b);
    (void)fs_cfg;
    auto* dep = g_nfc_b->isoDEP();
    // APDU の FWT も 100ms に短縮 (理由は select と同じ)。カードが S(WTX) で
    // 延長要求してきた場合は wtx_max まで自動で待つので安全
    const m5::nfc::isodep::policy_t fast_policy{/*fwt_ms=*/50U, /*wtx_max_ms=*/2000U, /*max_retries=*/0};

    uint8_t rx[64];
    uint16_t rx_len;

    rx_len = sizeof(rx);
    if (!dep->transceiveAPDU(rx, rx_len, kApduSelectMf, sizeof(kApduSelectMf), &fast_policy) || !sw_ok(rx, rx_len)) {
        g_nfc_b->deactivate();
        return -4;  // SELECT MF 失敗 (免許証以外の Type-B カードの可能性)
    }

    rx_len = sizeof(rx);
    if (!dep->transceiveAPDU(rx, rx_len, kApduSelectEf2F01, sizeof(kApduSelectEf2F01), &fast_policy) || !sw_ok(rx, rx_len)) {
        g_nfc_b->deactivate();
        return -5;  // SELECT EF 2F01 失敗
    }

    // EF は全17バイトなので Le=0x11 の一発読みで交付日・期限とも取れる
    // (Le=0x20 だと実カードが不安定になる事象を実機で確認済み)
    rx_len = sizeof(rx);
    if (!dep->transceiveAPDU(rx, rx_len, kApduReadBinary1, sizeof(kApduReadBinary1), &fast_policy) || !sw_ok(rx, rx_len)) {
        g_nfc_b->deactivate();
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

}  // namespace

extern "C" int nfc_shim_read_license_expiry(char* out_issue, int issue_cap, char* out_expiry, int expiry_cap)
{
    if (!g_ready || issue_cap < 9 || expiry_cap < 9) {
        return -1;
    }
    g_units.update();  // 理由は nfc_shim_poll_felica_idm のコメント参照
    ensure_mode(m5::nfc::NFC::B);

    // 安定化リトライ (2026-07-21 実機計測): 1回の呼び出し内で予算いっぱいまで
    // セッション全体を再試行する。リセットの方針が肝:
    //  - WUPB 無応答 (-2) ではリセットしない。フィールドを連続維持した方が
    //    カードの電源が安定し検出が速い (診断コードはリセット無しで安定検出。
    //    逆に -2 のたびにフィールド断を入れる方式は、設置中のカードの起動を
    //    繰り返し中断してしまい、数秒〜十数秒の全滅区間を作ることを実機確認)
    //  - セッション途中死 (-4/-5/-6) の直後だけリセットする (カードが
    //    READY/ACTIVE で固まり、チップ状態も汚れるため。リセット無しの
    //    再試行は無意味なことを実機確認)
    //  - 全滅のまま予算を使い切ったら最後に1回だけリセット (ATTRIB 失敗で
    //    READY スタックしたカードの保険。次の呼び出しまで ≥200ms 空くので
    //    再設定直後 WUPB 全滅問題は踏まない)
    constexpr uint32_t kBudgetMs = 500;
    const auto budget_end = m5::utility::millis() + kBudgetMs;
    int last_rc = -2;

    while (m5::utility::millis() <= budget_end) {
        const int rc = try_read_license_once(out_issue, out_expiry);
        if (rc == 0) {
            // 成功後もリセットしてカードを即 IDLE に戻す (2026-07-21): 読み取り後の
            // カードは deactivate に失敗して ACTIVE のまま沈黙するため、リセット
            // しないと次の検出まで丸2呼び出し (≒2秒) の死角ができる。リセット
            // すれば置きっぱなしのカードは毎サイクル再読取りでき (緑が維持される)、
            // 連続タップにも即応する
            reset_rf_field();
            return 0;
        }
        if (rc != -2) {
            last_rc = rc;  // 途中死の理由は最後のものを返す
            reset_rf_field();
            m5::utility::delay(60);  // 再設定直後の WUPB 不感時間の実験値 (100ms は安全確認済み、60ms を試行中)
        }
    }
    if (last_rc == -2) {
        // READY スタック解除の保険リセットは間引く (2026-07-21 実機で確認):
        // 毎呼び出し末尾に入れると、再設定後の不感時間 (数百 ms) が 0.5s
        // サイクルの大半を食い潰し、カードを置いていても十数秒 ATQB ゼロの
        // 無反応区間ができる。約3.5秒に1回で保険としては十分
        static uint32_t s_idle_calls = 0;
        if (++s_idle_calls >= 6) {
            s_idle_calls = 0;
            reset_rf_field();
        }
    }
    return last_rc;
}
