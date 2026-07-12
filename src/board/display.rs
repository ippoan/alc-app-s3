//! CoreS3 LCD (ILI9342C, 320x240) の初期化。
//!
//! ピン構成 (M5GFX board_M5StackCoreS3 準拠):
//!   SPI2: SCLK=G36 / MOSI=G37 / CS=G3 / DC=G35 (MISO と共用のため読み出し不可)
//!   RST: AW9523 P1_1 (power.rs で解放済み) / バックライト: AXP2101 DLDO1
//!
//! 色反転あり (invert)。色順・回転は実機確認で要調整の可能性あり。

use anyhow::{anyhow, Result};
use esp_idf_svc::hal::{
    delay::Delay,
    gpio::{AnyIOPin, Gpio3, Gpio35, Gpio36, Gpio37, Output, PinDriver},
    spi::{
        config::Config as SpiConfig, config::DriverConfig as SpiDriverConfig, config::MODE_0,
        SpiDeviceDriver, SpiDriver, SPI2,
    },
    units::Hertz,
};
use mipidsi::{
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder, Display, NoResetPin,
};

pub const LCD_W: i32 = 320;
pub const LCD_H: i32 = 240;

pub type Cs3Display = Display<
    SpiInterface<
        'static,
        SpiDeviceDriver<'static, SpiDriver<'static>>,
        PinDriver<'static, Output>,
    >,
    ILI9342CRgb565,
    NoResetPin,
>;

/// 設定値 (度) を mipidsi の Orientation へ変換
pub fn orientation_from_deg(deg: u16) -> Orientation {
    let rotation = match deg {
        90 => Rotation::Deg90,
        180 => Rotation::Deg180,
        270 => Rotation::Deg270,
        _ => Rotation::Deg0,
    };
    Orientation::new().rotate(rotation)
}

pub fn init(
    spi: SPI2<'static>,
    sclk: Gpio36<'static>,
    mosi: Gpio37<'static>,
    cs: Gpio3<'static>,
    dc: Gpio35<'static>,
    rotation_deg: u16,
) -> Result<Cs3Display> {
    let driver = SpiDriver::new(
        spi,
        sclk,
        mosi,
        Option::<AnyIOPin>::None, // MISO なし (G35 は DC と共用)
        &SpiDriverConfig::new(),
    )?;
    let spi_cfg = SpiConfig::new()
        .baudrate(Hertz(40_000_000))
        .data_mode(MODE_0);
    let device = SpiDeviceDriver::new(driver, Some(cs), &spi_cfg)?;
    let dc = PinDriver::output(dc)?;

    // SpiInterface のピクセル転送バッファ (静的に確保)
    let buf: &'static mut [u8] = Box::leak(Box::new([0u8; 4096]));
    let di = SpiInterface::new(device, dc, buf);

    let mut delay = Delay::new_default();
    Builder::new(ILI9342CRgb565, di)
        .display_size(LCD_W as u16, LCD_H as u16)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .orientation(orientation_from_deg(rotation_deg))
        .init(&mut delay)
        .map_err(|e| anyhow!("LCD 初期化失敗: {e:?}"))
}
