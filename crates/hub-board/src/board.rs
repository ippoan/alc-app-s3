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

/// 内部 I2C (G12/G11) を probe する。power::init の後 (周辺電源 ON 後) に呼ぶ
pub fn probe(i2c: &mut I2cDriver) -> Probe {
    Probe {
        rtc_present: acks(i2c, BM8563_ADDR),
        imu_present: acks(i2c, BMI270_ADDR),
    }
}
