//! CoreS3 統合ハブの周辺デバイス状態。
//!
//! 各 I/O モジュール (rs232 / lan / ble) が更新し、画面処理 (ui) が
//! ステータスバーおよびステータス詳細画面に反映する。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// ログ確認画面に保持する直近イベント数
pub const MAX_EVENTS: usize = 8;

#[derive(Default, Clone)]
pub struct HubStatus {
    /// 直近イベント (新しいものが末尾)。ログ確認画面に表示する
    pub events: VecDeque<String>,

    /// LAN Module 13.2 (W5500) のリンク状態 (lan.rs — 未実装のため常に false)
    pub lan_link: bool,

    /// RS232 (FC-1200) の最終受信時刻 [ms]。None = 起動後受信なし
    pub rs232_last_rx_ms: Option<u64>,

    /// 内蔵 BLE central の接続状態 (ble.rs — 未実装のため常に false)
    pub ble_connected: bool,
    /// 接続中の BLE デバイス名 (NT-100B / NBP-1BLE)
    pub ble_device: String,

    /// Wi-Fi STA の接続状態 (Improv Wi-Fi Serial で設定, wifi.rs が更新)
    pub wifi_connected: bool,
    /// Wi-Fi 接続時の IP アドレス
    pub wifi_ip: String,
}

impl HubStatus {
    /// 直近 `window_ms` 以内に RS232 受信があったか
    pub fn rs232_active(&self, now_ms: u64, window_ms: u64) -> bool {
        self.rs232_last_rx_ms
            .map_or(false, |t| now_ms.saturating_sub(t) < window_ms)
    }

    /// イベントログへ 1 行追加 (稼働時刻付き、直近 MAX_EVENTS 件を保持)
    pub fn push_event(&mut self, now_ms: u64, msg: &str) {
        let line = format!("{} {msg}", alc_hub_core::layout::fmt_uptime(now_ms));
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(line);
    }
}

pub type SharedStatus = Arc<Mutex<HubStatus>>;

/// 起動からの経過ミリ秒
pub fn now_ms() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 / 1000 }
}
