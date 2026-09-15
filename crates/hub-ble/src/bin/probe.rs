//! alc-hub-ble-probe — hub-ble の BLE central をそのまま Atom VoiceS3R で動かし、
//! `PROBE ` 行を serial に出す測定用 bin (issue #237)。
//!
//! scan / connect / bond は CoreS3 に載るのと同じ `alc_hub_ble::start` を通す。
//! ここは配線だけ: 測定値と UI コマンドの受け側は捨てる。

use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};

use alc_hub_common::status::HubStatus;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let status = Arc::new(Mutex::new(HubStatus::default()));
    let coex = Arc::new(alc_hub_core::coex::RadioCoex::new());
    let pair_flag = alc_hub_common::control::new_pair_flag();
    // 毎回まっさらな bond で測る: 起動直後に保存済み bond を消させる (既存の再ペアリング経路)
    pair_flag.store(true, Ordering::SeqCst);

    let (meas_tx, meas_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    std::thread::spawn(move || for _ in meas_rx {});
    std::thread::spawn(move || for _ in ui_rx {});

    println!("PROBE START");
    alc_hub_ble::start(status, meas_tx, ui_tx, coex, pair_flag)?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
