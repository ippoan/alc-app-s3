//! AtomS3 内蔵 GC9107 LCD (0.85インチ 128x128) のステータス表示。
//!
//! ピン (M5Stack 公式 docs + 実機確認済み):
//!   SPI3: SCK=G17 / MOSI=G21 / CS=G15 / DC=G33 / RST=G34 / BL=G16 (H で点灯)
//! W5500 が SPI2 (G5/G7/G8/G6) を使うため LCD は SPI3 に割り当てる。
//!
//! 表示設定は実機検証で確定したもの (2026-07-14、8色パターンで全色確認):
//! `display_offset(0, 32)` (M5GFX 実ソース準拠。31 だと可視最下行が
//! 未書き込みになりノイズが出る) + `ColorOrder::Bgr` + 色反転なし。
//! LCD の SPI は DMA 無しでよい (esp-idf-hal がソフト分割する。W5500 の
//! C ドライバと違い 64 バイト上限を踏まない — 実機で確認)。

use anyhow::{anyhow, Result};
use embedded_graphics::{
    mono_font::{ascii::FONT_8X13, MonoTextStyle, MonoTextStyleBuilder},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::Text,
};
use esp_idf_svc::hal::{
    delay::Delay,
    gpio::{AnyIOPin, Gpio15, Gpio16, Gpio17, Gpio21, Gpio33, Gpio34, Output, PinDriver},
    spi::{
        config::Config as SpiConfig, config::DriverConfig as SpiDriverConfig, config::MODE_0,
        SpiDeviceDriver, SpiDriver, SPI3,
    },
    units::Hertz,
};
use mipidsi::{
    interface::SpiInterface,
    models::GC9107,
    options::ColorOrder,
    Builder, Display,
};

pub const LCD_W: i32 = 128;
pub const LCD_H: i32 = 128;

type Gc9107Display = Display<
    SpiInterface<
        'static,
        SpiDeviceDriver<'static, SpiDriver<'static>>,
        PinDriver<'static, Output>,
    >,
    GC9107,
    PinDriver<'static, Output>,
>;

/// 直近描画した内容 (差分が無ければ再描画しない)
#[derive(Default, PartialEq, Clone)]
pub struct View {
    pub eth_up: bool,
    pub ip: String,
    /// 内部RAM の使用率 [%] (パーセント単位なので変化は緩やか = 再描画も稀)
    pub heap_used_pct: u8,
    /// device credential が NVS にあるか (無ければ WS は接続を試みない)
    pub paired: bool,
    /// cf-alc-recorder への WS 接続中か
    pub ws_up: bool,
}

pub struct Screen {
    display: Gc9107Display,
    // drop すると消灯するため保持し続ける
    _backlight: PinDriver<'static, Output>,
    last: Option<View>,
}

#[allow(clippy::too_many_arguments)]
pub fn init(
    spi: SPI3<'static>,
    sclk: Gpio17<'static>,
    mosi: Gpio21<'static>,
    cs: Gpio15<'static>,
    dc: Gpio33<'static>,
    rst: Gpio34<'static>,
    bl: Gpio16<'static>,
) -> Result<Screen> {
    let mut backlight = PinDriver::output(bl)?;
    backlight.set_high()?;

    let driver = SpiDriver::new(
        spi,
        sclk,
        mosi,
        Option::<AnyIOPin>::None,
        &SpiDriverConfig::new(),
    )?;
    let spi_cfg = SpiConfig::new()
        .baudrate(Hertz(20_000_000))
        .data_mode(MODE_0);
    let device = SpiDeviceDriver::new(driver, Some(cs), &spi_cfg)?;
    let dc = PinDriver::output(dc)?;
    let rst = PinDriver::output(rst)?;

    let buf: &'static mut [u8] = Box::leak(Box::new([0u8; 4096]));
    let di = SpiInterface::new(device, dc, buf);

    let mut delay = Delay::new_default();
    let mut display = Builder::new(GC9107, di)
        .display_size(LCD_W as u16, LCD_H as u16)
        .display_offset(0, 32)
        .color_order(ColorOrder::Bgr)
        .reset_pin(rst)
        .init(&mut delay)
        .map_err(|e| anyhow!("LCD 初期化失敗: {e:?}"))?;

    display
        .clear(Rgb565::BLACK)
        .map_err(|e| anyhow!("LCD クリア失敗: {e:?}"))?;

    let mut screen = Screen {
        display,
        _backlight: backlight,
        last: None,
    };
    screen.draw_chrome()?;
    Ok(screen)
}

impl Screen {
    /// 固定部分 (タイトルバー + バージョン) を描く。起動時に 1 回
    fn draw_chrome(&mut self) -> Result<()> {
        let d = &mut self.display;
        Rectangle::new(Point::new(0, 0), Size::new(LCD_W as u32, 18))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::new(4, 20, 16)))
            .draw(d)
            .map_err(|e| anyhow!("描画失敗: {e:?}"))?;
        let title = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);
        Text::new("PRINT HUB", Point::new(4, 13), title)
            .draw(d)
            .map_err(|e| anyhow!("描画失敗: {e:?}"))?;

        // バージョン (最下行、控えめの色)。128px = 16 文字に収める
        let ver = alc_hub_common::config::firmware_version_full();
        let ver: String = ver.chars().take(16).collect();
        let style = MonoTextStyle::new(&FONT_8X13, Rgb565::CSS_GRAY);
        Text::new(&ver, Point::new(4, LCD_H - 4), style)
            .draw(d)
            .map_err(|e| anyhow!("描画失敗: {e:?}"))?;
        Ok(())
    }

    /// 可変部分 (ETH 状態 / IP / ヒープ) を差分描画する。
    /// 塗り潰し→書き直しは blink するため行わず、背景色付きテキストを
    /// 固定幅 (15 桁 = 全行フル幅) で上書きする (CoreS3 ステータスバーと
    /// 同じ blink 回避方針)
    pub fn draw(&mut self, view: &View) -> Result<()> {
        if self.last.as_ref() == Some(view) {
            return Ok(());
        }
        let d = &mut self.display;

        // 15 桁 (テキスト開始 x=4 のため 128px に収まる最大幅) に左詰め整形。
        // 短い文字列でも行全体を上書きするので前回の残骸が残らない
        let pad = |s: &str| format!("{:<15.15}", s);
        let style = |color| {
            MonoTextStyleBuilder::new()
                .font(&FONT_8X13)
                .text_color(color)
                .background_color(Rgb565::BLACK)
                .build()
        };

        let (label, color) = if view.eth_up {
            ("ETH UP", Rgb565::GREEN)
        } else {
            ("ETH DOWN", Rgb565::RED)
        };
        Text::new(&pad(label), Point::new(4, 40), style(color))
            .draw(d)
            .map_err(|e| anyhow!("描画失敗: {e:?}"))?;

        let ip_line = if view.eth_up && !view.ip.is_empty() {
            view.ip.clone()
        } else {
            "LAN wait...".to_string()
        };
        Text::new(&pad(&ip_line), Point::new(4, 60), style(Rgb565::WHITE))
            .draw(d)
            .map_err(|e| anyhow!("描画失敗: {e:?}"))?;

        // 遠隔管理 (WS) の状態。未ペアリングはそもそも接続を試みないため
        // WS DOWN と区別して NO AUTH を出す (再フラッシュで NVS が消えた
        // ことに現地で気付けるように)
        let (ws_label, ws_color) = if !view.paired {
            ("NO AUTH", Rgb565::RED)
        } else if view.ws_up {
            ("WS UP", Rgb565::GREEN)
        } else {
            ("WS DOWN", Rgb565::YELLOW)
        };
        Text::new(&pad(ws_label), Point::new(4, 84), style(ws_color))
            .draw(d)
            .map_err(|e| anyhow!("描画失敗: {e:?}"))?;

        let heap = format!("MEM USED {}%", view.heap_used_pct);
        Text::new(&pad(&heap), Point::new(4, 104), style(Rgb565::CSS_LIGHT_GRAY))
            .draw(d)
            .map_err(|e| anyhow!("描画失敗: {e:?}"))?;

        self.last = Some(view.clone());
        Ok(())
    }
}
