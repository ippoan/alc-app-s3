//! alc-hub-cores3: M5Stack CoreS3 統合ハブ ファームウェア (画面処理)
//!
//! `ippoan/alc-app` の plan/cores3-hub-consolidation.md (issues #100 / #102 の
//! 参照元) に基づく、点呼キオスク向け CoreS3 統合ハブの画面処理実装。
//!
//! 構成:
//! - LCD (ILI9342C) + タッチ (FT5x06): 待機 / QR 表示 / 測定中 / 結果 / エラー画面
//! - USB-C (USB Serial/JTAG): ホスト (Windows PC / Android タブレット) との
//!   行指向プロトコル (host_link.rs)
//! - UART1 (G17/G18): RS232M Module → FC-1200 パススルー (rs232.rs)
//! - 内蔵 BLE central: NT-100B / NBP-1BLE 読み取り (ble.rs,
//!   ble-medical-gateway からの移植)
//! - LAN Module 13.2: 未実装スタブ (lan.rs)

mod ble;
mod board;
mod config;
mod host_link;
mod lan;
mod rs232;
mod settings;
mod status;
mod ui;

use std::sync::{mpsc, Arc, Mutex};

use anyhow::Result;
use esp_idf_svc::hal::{
    i2c::{config::Config as I2cConfig, I2cDriver},
    peripherals::Peripherals,
    units::Hertz,
};
use esp_idf_svc::nvs::EspDefaultNvsPartition;

use crate::settings::Settings;
use crate::status::{HubStatus, SharedStatus};

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("alc-hub-cores3 v{} 起動", config::FIRMWARE_VERSION);

    let p = Peripherals::take()?;

    // NVS (BLE スタックも使用) と永続設定 (画面向き)
    let nvs_partition = EspDefaultNvsPartition::take()?;
    let settings = Settings::new(nvs_partition)?;

    // 内部 I2C (SDA=G12 / SCL=G11): AXP2101 / AW9523 / FT5x06 (タッチ)
    let i2c_cfg = I2cConfig::new().baudrate(Hertz(400_000));
    let mut i2c = I2cDriver::new(p.i2c0, p.pins.gpio12, p.pins.gpio11, &i2c_cfg)?;

    // 電源 (LCD バックライト・リセット含む) → LCD の順で初期化
    board::power::init(&mut i2c)?;
    let display = board::display::init(
        p.spi2,
        p.pins.gpio36,
        p.pins.gpio37,
        p.pins.gpio3,
        p.pins.gpio35,
        settings.rotation(),
    )?;

    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));
    let (tx, rx) = mpsc::channel();

    host_link::start(tx, Arc::clone(&status), settings.clone())?;
    rs232::start(p.uart1, p.pins.gpio17, p.pins.gpio18, Arc::clone(&status))?;
    lan::start(Arc::clone(&status)); // TODO: W5500 実装 (lan.rs 参照)
    ble::start(Arc::clone(&status))?; // NT-100B / NBP-1BLE 読み取り (ble.rs)

    // UI ループ (メインタスクを占有, 戻らない)
    ui::run(display, i2c, rx, status)
}
