//! alc-hub-cores3: M5Stack CoreS3 統合ハブ ファームウェア (画面処理)
//!
//! `ippoan/alc-app` の plan/cores3-hub-consolidation.md (issues #100 / #102 の
//! 参照元) に基づく、点呼キオスク向け CoreS3 統合ハブの画面処理実装。
//!
//! クレート構成 (再コンパイル範囲の最小化と並列ビルドのための枝分かれ):
//!
//! ```text
//! hub-core (純粋) → hub-common (状態/設定/UIコマンド)
//!                     ├→ hub-ble   (体温計/血圧計)      ┐
//!                     ├→ hub-wifi  (Wi-Fi + Improv)     ├ 互いに独立 = 並列
//!                     ├→ hub-drivers (ホストリンク/RS232) ┘ (drivers→wifi)
//!                     └→ hub-ui    (画面。hub-board にも依存)
//! hub-board (ボード初期化, 独立葉)
//! 本クレート = main の配線のみ (ほぼ変更されない)
//! ```

use std::sync::{mpsc, Arc, Mutex};

use alc_hub_ble as ble;
use alc_hub_board as board;
use alc_hub_common::{
    config,
    settings::Settings,
    status::{now_ms, HubStatus, SharedStatus},
};
use alc_hub_drivers::{host_link, lan, rs232};
use alc_hub_ui as ui;
use alc_hub_wifi::{improv, wifi};
use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::{
    i2c::{config::Config as I2cConfig, I2cDriver},
    peripherals::Peripherals,
    units::Hertz,
};
use esp_idf_svc::nvs::EspDefaultNvsPartition;

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("alc-hub-cores3 v{} 起動", config::FIRMWARE_VERSION);

    let p = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;

    // NVS (BLE/Wi-Fi スタックも使用) と永続設定 (画面向き・Wi-Fi 認証情報)
    let nvs_partition = EspDefaultNvsPartition::take()?;
    let settings = Settings::new(nvs_partition.clone())?;

    // 内部 I2C (SDA=G12 / SCL=G11): AXP2101 / AW9523 / FT5x06 (タッチ)
    let i2c_cfg = I2cConfig::new().baudrate(Hertz(400_000));
    let mut i2c = I2cDriver::new(p.i2c0, p.pins.gpio12, p.pins.gpio11, &i2c_cfg)?;

    // 電源 (LCD バックライト・リセット含む) → LCD の順で初期化
    board::power::init(&mut i2c)?;
    let rotation = settings.rotation();
    let display = board::display::init(
        p.spi2,
        p.pins.gpio36,
        p.pins.gpio37,
        p.pins.gpio3,
        p.pins.gpio35,
        rotation,
    )?;

    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));
    let (tx, rx) = mpsc::channel();

    // Wi-Fi (Improv Wi-Fi Serial で設定。保存済みなら起動時に自動接続)
    let wifi = wifi::Wifi::new(p.modem, sysloop, nvs_partition, Arc::clone(&status))?;
    let coex = wifi.coex_handle();
    let saved_credentials = settings.wifi_credentials();
    let provisioned = saved_credentials.is_some();
    if let Some((ssid, pass)) = saved_credentials {
        let wifi = wifi.clone();
        let status_c = Arc::clone(&status);
        std::thread::Builder::new()
            .name("wifi_boot".into())
            .stack_size(8 * 1024)
            .spawn(move || match wifi.connect(&ssid, &pass) {
                Ok(ip) => log::info!("wifi: 起動時接続成功 {ip}"),
                Err(e) => {
                    log::warn!("wifi: 起動時接続失敗: {e:?}");
                    wifi.mark_disconnected();
                    if let Ok(mut st) = status_c.lock() {
                        st.push_event(now_ms(), "WiFi 接続失敗");
                    }
                }
            })?;
    }
    let improv =
        improv::Improv::new(settings.clone(), wifi.clone(), Arc::clone(&status), provisioned);

    host_link::start(tx.clone(), Arc::clone(&status), settings, wifi, improv)?;
    rs232::start(p.uart1, p.pins.gpio17, p.pins.gpio18, Arc::clone(&status))?;
    lan::start(Arc::clone(&status)); // TODO: W5500 実装 (lan.rs 参照)
    // NT-100B / NBP-1BLE 読み取り。Wi-Fi 接続/Improv セッション中は
    // BLE スキャンを一時停止する (RadioCoex)
    ble::start(Arc::clone(&status), tx, coex)?;

    // UI ループ (メインタスクを占有, 戻らない)
    ui::run(display, i2c, rx, status, rotation)
}
