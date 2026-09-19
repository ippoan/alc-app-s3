//! alc-hub-ble-probe — hub-ble の BLE central をそのまま Atom VoiceS3R で動かし、
//! `PROBE ADV` 行と `EVT` 行を serial に出す測定用 bin (issue #237)。
//!
//! scan / connect / bond は CoreS3 に載るのと同じ `alc_hub_ble::start` を通す。
//! ここは配線だけ: 測定値と UI コマンドの受け側は捨てる。

use std::sync::{mpsc, Arc, Mutex};

use alc_hub_common::settings::Settings;
use alc_hub_common::status::HubStatus;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // Omron を拾う前提の測定用 probe (issue #237)
    let status = Arc::new(Mutex::new(HubStatus {
        omron_bp: true,
        ..HubStatus::default()
    }));
    let coex = Arc::new(alc_hub_core::coex::RadioCoex::new());
    // 起動時に bond は消さない (消すと、ペアリング後の再接続で暗号化できない)。
    // 本番機は hub-drivers の console (`PAIR` → `handle_pair`) が立てるが、この測定用
    // bin は console を持たないので、シリアルから `PAIR` の 1 行だけ拾う口を置く
    let pair_flag = alc_hub_common::control::new_pair_flag();
    {
        use std::io::Read as _;
        // stdin のブロッキング読み出しに要る (hub-drivers console と同じ)
        unsafe {
            let mut cfg = esp_idf_svc::sys::usb_serial_jtag_driver_config_t {
                tx_buffer_size: 1024,
                rx_buffer_size: 1024,
            };
            esp_idf_svc::sys::usb_serial_jtag_driver_install(&mut cfg);
            esp_idf_svc::sys::esp_vfs_usb_serial_jtag_use_driver();
        }
        let pair_flag = Arc::clone(&pair_flag);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 32];
            let mut acc = String::new();
            loop {
                match std::io::stdin().lock().read(&mut chunk) {
                    Ok(0) => esp_idf_svc::hal::delay::FreeRtos::delay_ms(20),
                    Ok(n) => {
                        acc.push_str(&String::from_utf8_lossy(&chunk[..n]));
                        while let Some(nl) = acc.find('\n') {
                            let line: String = acc.drain(..=nl).collect();
                            if line.trim().eq_ignore_ascii_case("PAIR") {
                                pair_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                                println!("OK PAIR");
                            }
                        }
                    }
                    Err(_) => esp_idf_svc::hal::delay::FreeRtos::delay_ms(100),
                }
            }
        });
    }

    let (meas_tx, meas_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    std::thread::spawn(move || for _ in meas_rx {});
    std::thread::spawn(move || for _ in ui_rx {});

    // NVS は血圧計のボンド記録 (`bp_bond`) の読み書きに要る (Refs #249)
    let settings = Settings::new(esp_idf_svc::nvs::EspDefaultNvsPartition::take()?)?;

    println!("PROBE START");
    alc_hub_ble::start(status, meas_tx, ui_tx, coex, pair_flag, settings)?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
