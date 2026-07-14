//! alc-hub-atoms3-print: AtomS3 (C123) + Atomic PoE Base (A091) 印刷ブリッジ。
//!
//! 点呼記録 PDF を HTTP GET し、営業所プリンターの 9100/tcp (raw) へ
//! ストリーミング送信する常駐デバイス (ippoan/alc-app-s3#38、親: #37)。
//! CoreS3 統合ハブ (ルートの alc-hub-cores3) と hub-* クレート群を共有する。
//!
//! Milestone 0 (本コミット) のスコープ: W5500 Ethernet のリンクアップ確認のみ。
//! 印刷ロジック・ホストコンソール・WS 常時接続は後続 PR で結線する (計画は
//! issue #38 参照)。
//!
//! ハード構成:
//! - AtomS3 (SKU C123, ESP32-S3FN8): PSRAM 非搭載 (SPIRAM 系 sdkconfig は
//!   一切使わない)、8MB flash
//! - Atomic PoE Base (SKU A091): W5500 SPI Ethernet + PoE 給電。
//!   SCLK=G5 / MISO=G7 / MOSI=G8 / CS=G6、INT/RST 未配線 (polling)

mod console;
mod display;

use alc_hub_common::{
    config,
    settings::Settings,
    status::{HubStatus, SharedStatus},
};
use alc_hub_drivers::{eth_w5500, heap, ota};
use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::{
    delay::FreeRtos,
    peripherals::Peripherals,
    spi::{config::DriverConfig as SpiDriverConfig, Dma, SpiDriver},
};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!(
        "alc-hub-atoms3-print v{} 起動",
        config::firmware_version_full()
    );

    let p = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;

    // NVS (プリンター宛先等の永続設定)
    let nvs_partition = EspDefaultNvsPartition::take()?;
    let settings = Settings::new(nvs_partition)?;

    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));
    // ヒープ監視 (OOM 捕捉 + low-water 計測) は重いアロケーションより先に登録
    heap::start(Arc::clone(&status))?;

    // ホストコンソール (PING / STATUS / HEAP / OTA / PRINT / PRINTER ADDR)
    console::start(Arc::clone(&status), settings)?;

    // W5500 (Atomic PoE Base): SCLK=G5 / MISO=G7 / MOSI=G8 / CS=G6。
    // DMA 必須 — 無効だと SPI 転送が 64 バイト上限になり、Ethernet フレーム
    // (最大 ~1.5KB) の read/write が "spi transmit failed" で全滅する (実機で確認)
    let spi = SpiDriver::new(
        p.spi2,
        p.pins.gpio5,
        p.pins.gpio8,
        Some(p.pins.gpio7),
        &SpiDriverConfig::new().dma(Dma::Auto(4096)),
    )?;
    eth_w5500::start(spi, p.pins.gpio6.into(), sysloop, Arc::clone(&status))?;

    // 内蔵 LCD (SPI3): 現地で状態を体感できるステータス画面。
    // 初期化失敗しても本体機能 (印刷ブリッジ) は継続する
    let mut screen = match display::init(
        p.spi3,
        p.pins.gpio17,
        p.pins.gpio21,
        p.pins.gpio15,
        p.pins.gpio33,
        p.pins.gpio34,
        p.pins.gpio16,
    ) {
        Ok(s) => Some(s),
        Err(e) => {
            log::error!("LCD 初期化失敗 (表示なしで継続): {e:#}");
            None
        }
    };

    // 起動完了 = OTA rollback 解除 (CoreS3 と同じ安全装置、ota.rs 参照)
    ota::mark_boot_valid();

    // メインループ: 状態を LCD に反映 (差分描画)。ホスト向けイベントは
    // eth_w5500 / heap スレッドが出す
    loop {
        FreeRtos::delay_ms(500);
        let Some(screen) = screen.as_mut() else {
            continue;
        };
        let view = status
            .lock()
            .map(|st| display::View {
                eth_up: st.lan_link,
                ip: st.lan_ip.clone(),
                heap_used_pct: if st.heap_total_int > 0 {
                    ((st.heap_total_int - st.heap_free_int.min(st.heap_total_int)) * 100
                        / st.heap_total_int) as u8
                } else {
                    0
                },
            })
            .unwrap_or_default();
        if let Err(e) = screen.draw(&view) {
            log::warn!("LCD 描画失敗: {e:#}");
        }
    }
}
