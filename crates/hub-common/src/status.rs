//! CoreS3 統合ハブの周辺デバイス状態。
//!
//! 各 I/O モジュール (rs232 / lan / ble) が更新し、画面処理 (ui) が
//! ステータスバーおよびステータス詳細画面に反映する。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// ログ確認画面に保持する直近イベント数
pub const MAX_EVENTS: usize = 8;

/// イベント/測定ログ 1 行の時刻ラベル。NTP 同期済みなら日本時間
/// "MM/DD HH:MM:SS"、未同期なら稼働時間 "HH:MM:SS"。全ログで共通に使う。
pub fn event_timestamp(now_ms: u64) -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| alc_hub_core::clock::format_jst(d.as_secs() as i64))
        .unwrap_or_else(|| alc_hub_core::layout::fmt_uptime(now_ms))
}

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

    /// cf-alc-recorder への WS 接続状態 (ws_uplink.rs が更新)
    pub ws_connected: bool,
    /// WS 送信キューの未 ack 件数 (`WS STATUS` 応答用)
    pub ws_queue_len: usize,
    /// WS 送信の最終採番 seq
    pub ws_last_seq: u64,

    /// 内部RAM の現在空き [bytes] (heap.rs が定期更新。0 = 未計測)
    pub heap_free_int: usize,
    /// 内部RAM の起動以来の最低空き (low-water mark) [bytes] (Refs #27)
    pub heap_min_int: usize,
    /// PSRAM の現在空き [bytes] (未搭載/無効なら 0)
    pub heap_free_psram: usize,
    /// 内部RAM のヒープ総量 [bytes] (使用率計算用。0 = 未計測)
    pub heap_total_int: usize,
    /// PSRAM のヒープ総量 [bytes] (未搭載/無効なら 0)
    pub heap_total_psram: usize,
}

impl HubStatus {
    /// 直近 `window_ms` 以内に RS232 受信があったか
    pub fn rs232_active(&self, now_ms: u64, window_ms: u64) -> bool {
        self.rs232_last_rx_ms
            .map_or(false, |t| now_ms.saturating_sub(t) < window_ms)
    }

    /// イベントログへ 1 行追加 (時刻ラベル付き、直近 MAX_EVENTS 件を保持)。
    /// 時刻は NTP 同期済みなら日本時間、未同期なら稼働時間 (event_timestamp)。
    pub fn push_event(&mut self, now_ms: u64, msg: &str) {
        let line = format!("{} {msg}", event_timestamp(now_ms));
        self.push_line(line);
    }

    /// 整形済みの 1 行をイベントログへ追加 (時刻の付け方を呼び出し側が決める
    /// 場合用。測定値は NTP 同期時に実時刻を付けるため recorder が使う)。
    pub fn push_line(&mut self, line: String) {
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
