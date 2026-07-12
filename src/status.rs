//! CoreS3 統合ハブの周辺デバイス状態。
//!
//! 各 I/O モジュール (rs232 / lan / ble) が更新し、画面処理 (ui) が
//! ステータスバーおよびステータス詳細画面に反映する。

use std::sync::{Arc, Mutex};

#[derive(Default, Clone)]
pub struct HubStatus {
    /// LAN Module 13.2 (W5500) のリンク状態 (lan.rs — 未実装のため常に false)
    pub lan_link: bool,

    /// RS232 (FC-1200) の最終受信時刻 [ms]。None = 起動後受信なし
    pub rs232_last_rx_ms: Option<u64>,

    /// 内蔵 BLE central の接続状態 (ble.rs — 未実装のため常に false)
    pub ble_connected: bool,
    /// 接続中の BLE デバイス名 (NT-100B / NBP-1BLE)
    pub ble_device: String,
}

impl HubStatus {
    /// 直近 `window_ms` 以内に RS232 受信があったか
    pub fn rs232_active(&self, now_ms: u64, window_ms: u64) -> bool {
        self.rs232_last_rx_ms
            .map_or(false, |t| now_ms.saturating_sub(t) < window_ms)
    }
}

pub type SharedStatus = Arc<Mutex<HubStatus>>;

/// 起動からの経過ミリ秒
pub fn now_ms() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 / 1000 }
}
