//! Wi-Fi と BLE の電波コエグジスト調停 (純粋ロジック)。
//!
//! ESP32-S3 は Wi-Fi と BLE が 2.4GHz 無線を共有するため、Wi-Fi の接続/
//! スキャン中に BLE スキャンが走ると電波の取り合いでアソシエーションが
//! 遅れ、ESP Web Tools の Improv ダイアログ (待ち約10秒) がタイムアウト
//! しやすい。本モジュールは「BLE を今止めるべきか」の判定だけを持ち、
//! 実際の停止は hub-ble のスキャンループが行う。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Default)]
pub struct RadioCoex {
    /// Wi-Fi の接続/スキャンが進行中
    wifi_busy: AtomicBool,
    /// この時刻 (稼働 ms) まで BLE スキャンを止める (Improv セッション中など)
    ble_pause_until_ms: AtomicU64,
}

impl RadioCoex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_wifi_busy(&self, busy: bool) {
        self.wifi_busy.store(busy, Ordering::SeqCst);
    }

    /// now から ms の間 BLE スキャンを止める。既により長い停止が予約されて
    /// いる場合は短縮しない (延長のみ)
    pub fn pause_ble_for(&self, now_ms: u64, ms: u64) {
        self.ble_pause_until_ms
            .fetch_max(now_ms.saturating_add(ms), Ordering::SeqCst);
    }

    /// BLE スキャンを今止めるべきか
    pub fn ble_should_pause(&self, now_ms: u64) -> bool {
        self.wifi_busy.load(Ordering::SeqCst)
            || now_ms < self.ble_pause_until_ms.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_not_paused() {
        let c = RadioCoex::new();
        assert!(!c.ble_should_pause(0));
    }

    #[test]
    fn wifi_busy_pauses_ble() {
        let c = RadioCoex::new();
        c.set_wifi_busy(true);
        assert!(c.ble_should_pause(0));
        c.set_wifi_busy(false);
        assert!(!c.ble_should_pause(0));
    }

    #[test]
    fn pause_window_expires() {
        let c = RadioCoex::new();
        c.pause_ble_for(1_000, 500);
        assert!(c.ble_should_pause(1_400));
        assert!(!c.ble_should_pause(1_500));
    }

    #[test]
    fn later_shorter_pause_does_not_shrink() {
        let c = RadioCoex::new();
        c.pause_ble_for(1_000, 10_000); // 11_000 まで
        c.pause_ble_for(2_000, 1_000); // 3_000 — 短いので無視
        assert!(c.ble_should_pause(10_999));
        assert!(!c.ble_should_pause(11_000));
    }
}
