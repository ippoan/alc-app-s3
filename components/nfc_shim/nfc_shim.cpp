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

#include <driver/i2c_master.h>

#include <cstdio>
#include <cstring>

namespace {

m5::unit::UnitUnified g_units;
m5::unit::UnitNFC g_unit{};
bool g_ready = false;

// EF 2F01 (共通データ要素) の READ BINARY レスポンス内オフセット。
// plan/nfc-card-identity.md 記載 (ippoan/AlcoholChecker の NfcReader.kt 実装に準拠):
//   offset 10〜: 交付日 (BCD 4バイト = "YYYYMMDD")
//   offset 18〜: 有効期限 (BCD 4バイト)
// ただし同 doc が例示する READ BINARY の Le=0x11(17) では offset18 まで届かないため
// (17バイトだと index 0-16 まで)、本実装は Le=0x20(32) で 1 回で両方を読む。
// 実カードでの EF 長がこれより短い場合の挙動 (SW 6281/6282 等) は実機で要確認。
constexpr uint8_t kApduSelectMf[]    = {0x00, 0xA4, 0x00, 0x00};
constexpr uint8_t kApduSelectEf2F01[] = {0x00, 0xA4, 0x02, 0x0C, 0x02, 0x2F, 0x01};
constexpr uint8_t kApduReadBinary[]   = {0x00, 0xB0, 0x00, 0x00, 0x20};
constexpr int kIssueDateOffset  = 10;
constexpr int kExpiryDateOffset = 18;
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
    auto cfg  = g_unit.config();
    cfg.mode  = m5::nfc::NFC::F;  // 起動時の既定は NFC-F。license 読み取り時に NFCLayerB へ切替える
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
    g_ready = true;
    return 0;
}

extern "C" int nfc_shim_poll_felica_idm(char* out_hex, int out_cap)
{
    if (!g_ready || out_cap < 17) {
        return -1;
    }
    m5::nfc::NFCLayerF nfc_f{g_unit};
    m5::nfc::f::PICC picc{};
    if (!nfc_f.detect(picc, /*timeout_ms=*/100U)) {
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

extern "C" int nfc_shim_read_license_expiry(char* out_issue, int issue_cap, char* out_expiry, int expiry_cap)
{
    if (!g_ready || issue_cap < 9 || expiry_cap < 9) {
        return -1;
    }

    // NFC-B へモード切替 (免許証は Type-B)
    auto cfg = g_unit.config();
    cfg.mode = m5::nfc::NFC::B;
    g_unit.config(cfg);

    m5::nfc::NFCLayerB nfc_b{g_unit};
    m5::nfc::b::PICC picc{};
    if (!nfc_b.detect(picc)) {
        return -2;  // カード無し
    }
    if (!nfc_b.select(picc)) {
        return -3;  // ATTRIB 失敗
    }

    uint8_t rx[64];
    uint16_t rx_len;

    rx_len = sizeof(rx);
    if (!nfc_b.transceive(rx, rx_len, kApduSelectMf, sizeof(kApduSelectMf), 100U) || !sw_ok(rx, rx_len)) {
        nfc_b.deactivate();
        return -4;  // SELECT MF 失敗 (免許証以外の Type-B カードの可能性)
    }

    rx_len = sizeof(rx);
    if (!nfc_b.transceive(rx, rx_len, kApduSelectEf2F01, sizeof(kApduSelectEf2F01), 100U) || !sw_ok(rx, rx_len)) {
        nfc_b.deactivate();
        return -5;  // SELECT EF 2F01 失敗
    }

    rx_len = sizeof(rx);
    if (!nfc_b.transceive(rx, rx_len, kApduReadBinary, sizeof(kApduReadBinary), 100U) || !sw_ok(rx, rx_len)) {
        nfc_b.deactivate();
        return -6;  // READ BINARY 失敗
    }
    nfc_b.deactivate();

    // rx_len にはデータ本体 + 末尾2バイトSW が含まれる
    const int data_len = static_cast<int>(rx_len) - 2;
    if (data_len < kExpiryDateOffset + kBcdFieldLen) {
        return -7;  // 想定より短い (EF 長が事前想定と違う。実機で要再調整)
    }

    bcd4_to_yyyymmdd(rx + kIssueDateOffset, out_issue);
    bcd4_to_yyyymmdd(rx + kExpiryDateOffset, out_expiry);
    return 0;
}
