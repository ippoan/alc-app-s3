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
    prelude::*,
    spi::{
        config::Config as SpiConfig, config::DriverConfig as SpiDriverConfig, SpiDeviceDriver,
        SpiDriver, SPI2,
    },
};
use mipidsi::{
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
    Builder, Display, NoResetPin,
};

pub const LCD_W: i32 = 320;
pub const LCD_H: i32 = 240;

pub type Cs3Display = Display<
    SpiInterface<
        'static,
        SpiDeviceDriver<'static, SpiDriver<'static>>,
        PinDriver<'static, Gpio35, Output>,
    >,
    ILI9342CRgb565,
    NoResetPin,
>;

pub fn init(
    spi: SPI2,
    sclk: Gpio36,
    mosi: Gpio37,
    cs: Gpio3,
    dc: Gpio35,
) -> Result<Cs3Display> {
    let driver = SpiDriver::new(
        spi,
        sclk,
        mosi,
        Option::<AnyIOPin>::None, // MISO なし (G35 は DC と共用)
        &SpiDriverConfig::new(),
    )?;
    let spi_cfg = SpiConfig::new()
        .baudrate(40.MHz().into())
        .data_mode(embedded_hal::spi::MODE_0);
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
        .init(&mut delay)
        .map_err(|e| anyhow!("LCD 初期化失敗: {e:?}"))
}
