//! ボード種別判定のための内部 I2C probe。
//!
//! CoreS3 SE は RTC (BM8563, 0x51) と IMU (BMI270, 0x69) を持たない
//! (plan/cores3-hub-consolidation.md「次期構成」)。ここでは各アドレスが ack
//! するかだけを返し、種別の判定は純粋ロジック側 (`alc_hub_core::board::
//! BoardKind::from_probe`) に任せる — 本クレートは他クレートに依存しない
//! 独立葉のまま保つ。

use esp_idf_svc::hal::{delay::BLOCK, i2c::I2cDriver};

/// BM8563 RTC (CoreS3 のみ)
const BM8563_ADDR: u8 = 0x51;
/// BMI270 IMU (CoreS3 のみ)
const BMI270_ADDR: u8 = 0x69;

/// 内部 I2C 上のオプション部品の有無
#[derive(Debug, Clone, Copy, Default)]
pub struct Probe {
    pub rtc_present: bool,
    pub imu_present: bool,
}

fn acks(i2c: &mut I2cDriver, addr: u8) -> bool {
    // レジスタ 0 の 1 バイト読み。中身は見ない — ack/NAK だけが欲しい
    let mut buf = [0u8; 1];
    i2c.write_read(addr, &[0x00], &mut buf, BLOCK).is_ok()
}

/// BM8563 の時刻レジスタの先頭 (秒) と長さ。中身の解釈は `alc_hub_core::rtc`
const BM8563_TIME_REG: u8 = 0x02;
const BM8563_TIME_LEN: usize = 7;

/// BM8563 の時刻レジスタ (秒〜年の 7 バイト) を読む。RTC の無い SE ではエラー
pub fn rtc_read(i2c: &mut I2cDriver) -> Result<[u8; BM8563_TIME_LEN], esp_idf_svc::sys::EspError> {
    let mut regs = [0u8; BM8563_TIME_LEN];
    i2c.write_read(BM8563_ADDR, &[BM8563_TIME_REG], &mut regs, BLOCK)?;
    Ok(regs)
}

/// BM8563 の時刻レジスタへ書く (秒から 7 バイト。VL も消える)
pub fn rtc_write(i2c: &mut I2cDriver, regs: &[u8; BM8563_TIME_LEN]) -> Result<(), esp_idf_svc::sys::EspError> {
    let mut frame = [0u8; BM8563_TIME_LEN + 1];
    frame[0] = BM8563_TIME_REG;
    frame[1..].copy_from_slice(regs);
    i2c.write(BM8563_ADDR, &frame, BLOCK)
}

/// 内部 I2C (G12/G11) を probe する。power::init の後 (周辺電源 ON 後) に呼ぶ
pub fn probe(i2c: &mut I2cDriver) -> Probe {
    Probe {
        rtc_present: acks(i2c, BM8563_ADDR),
        imu_present: acks(i2c, BMI270_ADDR),
    }
}
