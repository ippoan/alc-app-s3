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

use alc_hub_common::{
    config,
    status::{HubStatus, SharedStatus},
};
use alc_hub_drivers::{eth_w5500, heap, ota};
use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::{
    delay::FreeRtos,
    peripherals::Peripherals,
    spi::{config::DriverConfig as SpiDriverConfig, SpiDriver},
};
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

    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));
    // ヒープ監視 (OOM 捕捉 + low-water 計測) は重いアロケーションより先に登録
    heap::start(Arc::clone(&status))?;

    // W5500 (Atomic PoE Base): SCLK=G5 / MISO=G7 / MOSI=G8 / CS=G6
    let spi = SpiDriver::new(
        p.spi2,
        p.pins.gpio5,
        p.pins.gpio8,
        Some(p.pins.gpio7),
        &SpiDriverConfig::new(),
    )?;
    eth_w5500::start(spi, p.pins.gpio6.into(), sysloop, Arc::clone(&status))?;

    // 起動完了 = OTA rollback 解除 (CoreS3 と同じ安全装置、ota.rs 参照)
    ota::mark_boot_valid();

    // Milestone 0 はリンク監視のみ — eth_w5500 スレッドが EVT ETH_* を出す
    loop {
        FreeRtos::delay_ms(1_000);
    }
}
