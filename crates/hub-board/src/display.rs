//! CoreS3 LCD (ILI9342C, 320x240) の初期化。
//!
//! ピン構成 (M5GFX board_M5StackCoreS3 準拠):
//!   SPI2: SCLK=G36 / MOSI=G37 / CS=G3 / DC=G35
//!   RST: AW9523 P1_1 (power.rs で解放済み) / バックライト: AXP2101 DLDO1
//!
//! # G35 の二役 (LCD DC ↔ M-Bus MISO) とバス共有
//!
//! CoreS3 は LCD の DC と M-Bus SPI の MISO が **同じ G35** に配線されており、
//! SPI2 バス (SCLK=G36 / MOSI=G37) も M-Bus とそのまま共有される。LAN Module
//! 13.2 (W5500、lan.rs) を同バスに載せるため、LCD 書き込みは毎回:
//!
//! 1. `spi_device_acquire_bus` でバスを占有 (進行中の W5500 転送の完了を待ち、
//!    新規転送をブロック)
//! 2. G35 を GPIO 出力に切替えて DC として駆動
//! 3. コマンド/ピクセルを送信
//! 4. G35 を入力に戻し (W5500 の MISO 出力と衝突しないよう解放)、バスを返す
//!
//! という手順を踏む (SharedDcInterface)。G35 の入力マトリクス (MISO ルーティング)
//! は方向切替では壊れない。占有中は W5500 側の転送が待たされるが、LCD 全面
//! 描画でも数十 ms 程度で、Ethernet はリトライ/キューで吸収される。
//!
//! 色反転あり (invert)。色順・回転は実機確認で要調整の可能性あり。

use anyhow::{anyhow, Result};
use esp_idf_svc::hal::{
    delay::{Delay, BLOCK},
    gpio::Gpio3,
    spi::{
        config::Config as SpiConfig, config::MODE_0, SpiDeviceDriver, SpiDriver,
    },
    units::Hertz,
};
use esp_idf_svc::sys;
use mipidsi::{
    interface::Interface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder, Display, NoResetPin,
};

pub const LCD_W: i32 = 320;
pub const LCD_H: i32 = 240;

/// SpiInterface 相当のピクセル転送バッファサイズ。DMA 対応領域に確保する
const XFER_BUF_LEN: usize = 4096;

pub type Cs3Display = Display<SharedDcInterface, ILI9342CRgb565, NoResetPin>;

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

/// G35 二役対応の mipidsi Interface (モジュール docコメント参照)。
/// mipidsi::interface::SpiInterface 相当のバッファリング送信に、
/// バス占有と G35 の方向切替を加えたもの
pub struct SharedDcInterface {
    dev: SpiDeviceDriver<'static, &'static SpiDriver<'static>>,
    /// 転送バッファ (DMA 対応領域、leak 済み)
    buf: &'static mut [u8],
}

impl SharedDcInterface {
    const DC: sys::gpio_num_t = 35;

    /// バスを占有し G35 を DC (出力) として確保する
    fn claim(&mut self) {
        unsafe {
            // 進行中の W5500 転送完了を待って占有 (BLOCK = 無期限)
            sys::spi_device_acquire_bus(self.dev.device(), BLOCK);
            sys::gpio_set_direction(Self::DC, sys::gpio_mode_t_GPIO_MODE_OUTPUT);
        }
    }

    /// G35 を入力 (MISO) に戻しバスを解放する
    fn release(&mut self) {
        unsafe {
            sys::gpio_set_direction(Self::DC, sys::gpio_mode_t_GPIO_MODE_INPUT);
            sys::spi_device_release_bus(self.dev.device());
        }
    }

    fn set_dc(&mut self, high: bool) {
        unsafe {
            sys::gpio_set_level(Self::DC, u32::from(high));
        }
    }
}

impl Interface for SharedDcInterface {
    type Word = u8;
    type Error = esp_idf_svc::sys::EspError;

    fn send_command(&mut self, command: u8, args: &[u8]) -> Result<(), Self::Error> {
        self.claim();
        self.set_dc(false);
        let r = self
            .dev
            .write(&[command])
            .and_then(|_| {
                self.set_dc(true);
                self.dev.write(args)
            });
        self.release();
        r
    }

    fn send_pixels<const N: usize>(
        &mut self,
        pixels: impl IntoIterator<Item = [Self::Word; N]>,
    ) -> Result<(), Self::Error> {
        self.claim();
        self.set_dc(true);
        let mut arrays = pixels.into_iter();
        let mut result = Ok(());
        let mut done = false;
        while !done {
            let mut i = 0;
            for chunk in self.buf.chunks_exact_mut(N) {
                if let Some(array) = arrays.next() {
                    chunk.copy_from_slice(&array);
                    i += N;
                } else {
                    done = true;
                    break;
                }
            }
            if let Err(e) = self.dev.write(&self.buf[..i]) {
                result = Err(e);
                break;
            }
        }
        self.release();
        result
    }

    fn send_repeated_pixel<const N: usize>(
        &mut self,
        pixel: [Self::Word; N],
        count: u32,
    ) -> Result<(), Self::Error> {
        self.claim();
        self.set_dc(true);
        let fill_count = core::cmp::min(count, (self.buf.len() / N) as u32);
        let filled_len = fill_count as usize * N;
        for chunk in self.buf[..filled_len].chunks_exact_mut(N) {
            chunk.copy_from_slice(&pixel);
        }
        let mut count = count;
        let mut result = Ok(());
        while count >= fill_count && fill_count > 0 {
            if let Err(e) = self.dev.write(&self.buf[..filled_len]) {
                result = Err(e);
                break;
            }
            count -= fill_count;
        }
        if result.is_ok() && count != 0 {
            result = self.dev.write(&self.buf[..(count as usize * N)]).map(|_| ());
        }
        self.release();
        result
    }
}

/// DMA 対応領域 (内部 RAM) に転送バッファを確保する。
/// PSRAM 有効構成では Box/Vec が PSRAM に載ることがあり、SPI DMA から
/// 参照できないため heap_caps_malloc(MALLOC_CAP_DMA) で明示する
fn alloc_dma_buf(len: usize) -> Result<&'static mut [u8]> {
    let p = unsafe {
        sys::heap_caps_malloc(len, sys::MALLOC_CAP_DMA | sys::MALLOC_CAP_8BIT) as *mut u8
    };
    if p.is_null() {
        return Err(anyhow!("LCD 転送バッファの確保失敗 ({len}B, DMA 対応領域)"));
    }
    Ok(unsafe { core::slice::from_raw_parts_mut(p, len) })
}

/// LCD を初期化する。`spi` は W5500 (lan.rs) と共有する M-Bus/SPI2 バス
/// (main.rs が MISO=G35 込みで構築して leak したもの)。
///
/// DC (G35) は SpiDriver が MISO として所有しているため、本関数は
/// PinDriver を作らず raw GPIO 操作 (SharedDcInterface) で方向を切替える
pub fn init(
    spi: &'static SpiDriver<'static>,
    cs: Gpio3<'static>,
    rotation_deg: u16,
) -> Result<Cs3Display> {
    let spi_cfg = SpiConfig::new()
        .baudrate(Hertz(40_000_000))
        .data_mode(MODE_0);
    let dev = SpiDeviceDriver::new(spi, Some(cs), &spi_cfg)?;

    let di = SharedDcInterface {
        dev,
        buf: alloc_dma_buf(XFER_BUF_LEN)?,
    };

    let mut delay = Delay::new_default();
    Builder::new(ILI9342CRgb565, di)
        .display_size(LCD_W as u16, LCD_H as u16)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .orientation(orientation_from_deg(rotation_deg))
        .init(&mut delay)
        .map_err(|e| anyhow!("LCD 初期化失敗: {e:?}"))
}
