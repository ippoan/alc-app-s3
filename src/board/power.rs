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
    write_reg(i2c, AXP2101_ADDR, 0x90, 0xBF)?; // LDO 有効化 (bit7 = DLDO1: LCD バックライト)
    write_reg(i2c, AXP2101_ADDR, 0x92, 13)?; // ALDO1 1.8V (AW88298 スピーカー AMP)
    write_reg(i2c, AXP2101_ADDR, 0x93, 28)?; // ALDO2 3.3V (ES7210 マイク ADC)
    write_reg(i2c, AXP2101_ADDR, 0x94, 28)?; // ALDO3 3.3V (カメラ)
    write_reg(i2c, AXP2101_ADDR, 0x95, 28)?; // ALDO4 3.3V (TF カード)
    write_reg(i2c, AXP2101_ADDR, 0x27, 0x00)?; // PowerKey Hold=1s / PowerOff=4s
    write_reg(i2c, AXP2101_ADDR, 0x69, 0x11)?; // CHGLED 設定
    write_reg(i2c, AXP2101_ADDR, 0x10, 0x30)?; // PMU 共通設定

    // バックライト最大 (DLDO1 = 3.3V)。set_backlight() でも変更可
    write_reg(i2c, AXP2101_ADDR, 0x99, 28)?;

    // --- AW9523: ポート初期値・方向 (P1_1 = LCD RST を H で解放) ---
    write_reg(i2c, AW9523_ADDR, 0x02, 0b0000_0101)?; // P0 出力値
    write_reg(i2c, AW9523_ADDR, 0x03, 0b0000_0011)?; // P1 出力値 (bit1: LCD RST = H)
    write_reg(i2c, AW9523_ADDR, 0x04, 0b0001_1000)?; // P0 方向 (0 = 出力)
    write_reg(i2c, AW9523_ADDR, 0x05, 0b0000_1100)?; // P1 方向
    write_reg(i2c, AW9523_ADDR, 0x11, 0b0001_0000)?; // GCR: P0 push-pull
    write_reg(i2c, AW9523_ADDR, 0x12, 0xFF)?; // P0 LED モード無効
    write_reg(i2c, AW9523_ADDR, 0x13, 0xFF)?; // P1 LED モード無効

    Ok(())
}

/// バックライト輝度 (0-100%)。AXP2101 DLDO1 の電圧 (2.5V-3.3V) で制御する。
#[allow(dead_code)]
pub fn set_backlight(i2c: &mut I2cDriver, percent: u8) -> Result<()> {
    let percent = percent.min(100) as u32;
    if percent == 0 {
        // DLDO1 を最低電圧に (完全消灯は 0x90 bit7 クリアだが他 LDO 設定を保持
        // したまま最低輝度に落とす方が安全)
        return write_reg(i2c, AXP2101_ADDR, 0x99, 20);
    }
    // 20 (2.5V) 〜 28 (3.3V) にマップ
    let step = 20 + (percent * 8) / 100;
    write_reg(i2c, AXP2101_ADDR, 0x99, step as u8)
}
