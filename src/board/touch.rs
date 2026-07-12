//! FT5x06 系タッチコントローラ (I2C 0x38) の最小限ポーリング実装。
//!
//! CoreS3 のタッチは内部 I2C (SDA=G12 / SCL=G11) に接続。INT (G21) は使わず
//! UI ループから定期ポーリングする。

use esp_idf_svc::hal::{delay::BLOCK, i2c::I2cDriver};

const FT5X06_ADDR: u8 = 0x38;

#[derive(Clone, Copy, Debug)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
}

/// 現在のタッチ位置。非タッチ時は None。
pub fn read(i2c: &mut I2cDriver) -> Option<TouchPoint> {
    // reg 0x02: タッチ点数, 0x03-0x06: 1点目の XY
    let mut buf = [0u8; 5];
    i2c.write_read(FT5X06_ADDR, &[0x02], &mut buf, BLOCK).ok()?;
    let touches = buf[0] & 0x0F;
    if touches == 0 || touches == 0x0F {
        return None;
    }
    let x = u16::from(buf[1] & 0x0F) << 8 | u16::from(buf[2]);
    let y = u16::from(buf[3] & 0x0F) << 8 | u16::from(buf[4]);
    Some(TouchPoint { x, y })
}
