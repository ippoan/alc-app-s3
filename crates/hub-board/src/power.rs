//! AXP2101 (PMIC) / AW9523 (IOエキスパンダ) の初期化。
//!
//! CoreS3 では LCD リセットが AW9523 P1_1、LCD バックライトが AXP2101 DLDO1
//! に接続されており、GPIO 直結ではない。初期化シーケンスは M5Unified の
//! CoreS3 起動処理を移植 (実機未検証)。

use anyhow::{Context, Result};
use esp_idf_svc::hal::{delay::BLOCK, i2c::I2cDriver};

const AXP2101_ADDR: u8 = 0x34;
const AW9523_ADDR: u8 = 0x58;

fn write_reg(i2c: &mut I2cDriver, addr: u8, reg: u8, value: u8) -> Result<()> {
    i2c.write(addr, &[reg, value], BLOCK)
        .with_context(|| format!("I2C write addr=0x{addr:02X} reg=0x{reg:02X}"))?;
    Ok(())
}

/// 電源系の初期化。LCD を含む周辺電源を有効化し、LCD リセットを解放する。
pub fn init(i2c: &mut I2cDriver) -> Result<()> {
    // --- AXP2101: LDO 有効化と電圧設定 (M5Unified CoreS3 シーケンス準拠) ---
    write_reg(i2c, AXP2101_ADDR, 0x90, LDO_ENABLE_ALL)?; // LDO 有効化 (bit7 = DLDO1: LCD バックライト)
    // ALDO1 3.3V (AW88298 スピーカー AMP)。元は 1.8V だったが、実機で AW88298
    // SYSST.UVLS (VDD<2.8V) が終始立ったまま (BLDO1/2 を 3.3V に上げても変化なし)
    // だったため、AW88298 の VDD/DVDD ピンは ALDO1 (この既存コメントの通り) の
    // 可能性が高いと判断し 3.3V に引き上げる実験 (issue #101 PR2、2026-07-21)
    write_reg(i2c, AXP2101_ADDR, 0x92, 28)?;
    write_reg(i2c, AXP2101_ADDR, 0x93, 28)?; // ALDO2 3.3V (ES7210 マイク ADC)
    write_reg(i2c, AXP2101_ADDR, 0x94, 28)?; // ALDO3 3.3V (カメラ)
    write_reg(i2c, AXP2101_ADDR, 0x95, 28)?; // ALDO4 3.3V (TF カード)
    // BLDO1/2: 電圧レジスタ (0x96/0x97) を明示せず EFUSE 既定値 (datasheet Table 6-1:
    // BLDO1=1.8V, BLDO2=2.8V) のままだった。BLDO2 の 2.8V は AW88298 の UVLS 閾値
    // (VDD<2.8V で undervoltage) ぎりぎりで、実機で SYSST.UVLS=1 (無音) を確認
    // (issue #101 PR2、2026-07-21)。安全側の 3.3V に上げる
    write_reg(i2c, AXP2101_ADDR, 0x96, 28)?; // BLDO1 3.3V
    write_reg(i2c, AXP2101_ADDR, 0x97, 28)?; // BLDO2 3.3V (AW88298 スピーカー AMP VDD の疑い)
    write_reg(i2c, AXP2101_ADDR, 0x27, 0x00)?; // PowerKey Hold=1s / PowerOff=4s
    write_reg(i2c, AXP2101_ADDR, 0x69, 0x11)?; // CHGLED 設定
    write_reg(i2c, AXP2101_ADDR, 0x10, 0x30)?; // PMU 共通設定

    // バックライト最大 (DLDO1 = 3.3V)。set_backlight() でも変更可
    write_reg(i2c, AXP2101_ADDR, 0x99, 28)?;

    // バッテリー/電源 ADC を有効化 (0x30: bit0=Vbat, bit2=Vbus, bit3=Vsys)。
    // bit0=VBAT / bit1=TS / bit2=VBUS / bit3=VSYS。M5Unified CoreS3 と同値 0x0F
    // で全チャネル有効化する (立てないと read_status() の電圧が読めない)。Refs #50
    write_reg(i2c, AXP2101_ADDR, 0x30, 0x0F)?;

    // --- AW9523: ポート初期値・方向 (P1_1 = LCD RST を H で解放) ---
    // bit1 (BUS_EN) を立てて M-Bus へ 5V を出す (M5Unified setExtOutput(true) 相当)。
    // 立てないとスタックモジュール (RS232M/LAN 13.2) が無電源になる
    write_reg(i2c, AW9523_ADDR, 0x02, 0b0000_0111)?; // P0 出力値 (bit1: BUS_EN = H)
    write_reg(i2c, AW9523_ADDR, 0x03, 0b1000_0011)?; // P1 出力値 (bit1: LCD RST = H, bit7: BOOST_EN = H)
    write_reg(i2c, AW9523_ADDR, 0x04, 0b0001_1000)?; // P0 方向 (0 = 出力)
    write_reg(i2c, AW9523_ADDR, 0x05, 0b0000_1100)?; // P1 方向
    write_reg(i2c, AW9523_ADDR, 0x11, 0b0001_0000)?; // GCR: P0 push-pull
    write_reg(i2c, AW9523_ADDR, 0x12, 0xFF)?; // P0 LED モード無効
    write_reg(i2c, AW9523_ADDR, 0x13, 0xFF)?; // P1 LED モード無効

    Ok(())
}

/// init() が 0x90 (LDO 有効化レジスタ) に書く値。bit7=DLDO1 (LCD バックライト)、
/// 他 bit は ALDO1-4 (スピーカー/マイク/カメラ/TF カード)
const LDO_ENABLE_ALL: u8 = 0xBF;

/// LCD バックライト (DLDO1) の有効/無効を切り替える。他 LDO (マイク/カメラ/
/// TF カード等) の電源はそのまま保持する — 影響するのは 0x90 の bit7 のみ。
/// 無効時も輝度レジスタ (0x99) は触らないため、再度有効化すると前回の
/// 輝度に戻る
#[allow(dead_code)]
pub fn set_backlight_enabled(i2c: &mut I2cDriver, enabled: bool) -> Result<()> {
    let reg = if enabled {
        LDO_ENABLE_ALL
    } else {
        LDO_ENABLE_ALL & !0x80
    };
    write_reg(i2c, AXP2101_ADDR, 0x90, reg)
}

/// バックライト輝度 (0-100%)。AXP2101 DLDO1 の電圧 (2.5V-3.3V) で制御する。
/// 完全消灯 (set_backlight_enabled(false)) だと本体が動作中か判別できる
/// 光が無くなるため、無操作時は最低輝度 (0%=2.5V) までに留める運用にしている
/// (画面焼け対策、ui/lib.rs 参照)
pub fn set_backlight(i2c: &mut I2cDriver, percent: u8) -> Result<()> {
    let percent = percent.min(100) as u32;
    // 20 (2.5V, 最低輝度) 〜 28 (3.3V, 最大輝度) にマップ
    let step = 20 + (percent * 8) / 100;
    write_reg(i2c, AXP2101_ADDR, 0x99, step as u8)
}

/// AXP2101 が測る電源/バッテリー状態のスナップショット。
/// 「外部給電は来ているのに Core が起動しない (brownout)」「充電できているか」
/// を実データで確認するための診断値 (Refs #50)。
#[derive(Debug, Clone, Copy, Default)]
pub struct PowerStatus {
    /// バッテリー残量 [%] (電圧から推定。M5Unified 準拠 — 0xA4 ゲージは CoreS3 で不定)
    pub battery_percent: u8,
    /// バッテリー電圧 [mV] (0x34/0x35、1mV/LSB)。0 = 未計測
    pub battery_mv: u16,
    /// VBUS (外部給電) が有効か (0x00 bit5)
    pub vbus_present: bool,
    /// バッテリーが接続されているか (0x00 bit3)
    pub battery_present: bool,
    /// 充電状態 (0x01 bits[6:5]): 0=待機/満充電, 1=充電中, 2=放電中
    pub charge_state: u8,
    /// ADC channel enable (0x30) の readback — VBAT(bit0) が立っているかの診断用
    pub adc_cfg: u8,
    /// フューエルゲージ 0xA4 の生値 (CoreS3 では不定。診断用に残す)
    pub gauge_raw: u8,
    /// 電圧レジスタ生値 (0x34, 0x35) — 実機での電圧デコード確認用
    pub volt_raw: (u8, u8),
    /// 生ステータス (0x00, 0x01) — 解釈の裏取り用
    pub status_raw: (u8, u8),
}

fn read_reg(i2c: &mut I2cDriver, addr: u8, reg: u8) -> Result<u8> {
    let mut buf = [0u8; 1];
    i2c.write_read(addr, &[reg], &mut buf, BLOCK)
        .with_context(|| format!("I2C read addr=0x{addr:02X} reg=0x{reg:02X}"))?;
    Ok(buf[0])
}

/// AXP2101 の電源/バッテリー状態を読む。レジスタ定義は M5Unified /
/// XPowersLib の AXP2101 に準拠 (電圧の目盛りは実機で要確認)。
pub fn read_status(i2c: &mut I2cDriver) -> Result<PowerStatus> {
    let s0 = read_reg(i2c, AXP2101_ADDR, 0x00)?; // PMU status1
    let s1 = read_reg(i2c, AXP2101_ADDR, 0x01)?; // PMU status2
    let adc_cfg = read_reg(i2c, AXP2101_ADDR, 0x30)?; // ADC enable readback
    let gauge_raw = read_reg(i2c, AXP2101_ADDR, 0xA4)?; // E-gauge % (CoreS3 で不定)
    // 電池電圧: 0x34[5:0]=上位 6bit, 0x35=下位 8bit の 14bit 値 (1mV/LSB)
    let vh = read_reg(i2c, AXP2101_ADDR, 0x34)?;
    let vl = read_reg(i2c, AXP2101_ADDR, 0x35)?;
    let mv = (((vh & 0x3F) as u16) << 8) | vl as u16;
    // 残量は電圧から推定する (M5Unified 準拠: 3.3V=0% 〜 4.1V=100% 線形 clamp)。
    // AXP2101 の E-gauge (0xA4) は CoreS3 で不定のため使わない。
    let percent = if mv <= 3300 {
        0
    } else if mv >= 4100 {
        100
    } else {
        (((mv - 3300) as u32 * 100) / 800) as u8
    };
    Ok(PowerStatus {
        battery_percent: percent,
        battery_mv: mv,
        vbus_present: (s0 & 0x20) != 0,
        battery_present: (s0 & 0x08) != 0,
        charge_state: (s1 >> 5) & 0x03,
        adc_cfg,
        gauge_raw,
        volt_raw: (vh, vl),
        status_raw: (s0, s1),
    })
}
