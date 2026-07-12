//! Wi-Fi と BLE の電波コエグジスト調停 (純粋ロジック)。
//!
//! ESP32-S3 は Wi-Fi と BLE が 2.4GHz 無線を共有するため、Wi-Fi の接続/
//! スキャン中に BLE スキャンが走ると電波の取り合いでアソシエーションが
//! 遅れ、ESP Web Tools の Improv ダイアログ (待ち約10秒) がタイムアウト
//! しやすい。本モジュールは「BLE を今止めるべきか」の判定だけを持ち、
//! 実際の停止は hub-ble のスキャンループが行う。
//!
//! 停止期限は秒粒度の AtomicU32 で保持する (Xtensa は 32bit で
//! AtomicU64 が存在しないため。u32 秒 ≈ 136 年で実用上十分)。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[derive(Default)]
pub struct RadioCoex {
    /// Wi-Fi の接続/スキャンが進行中
    wifi_busy: AtomicBool,
    /// この時刻 (稼働秒) まで BLE スキャンを止める (Improv セッション中など)
    ble_pause_until_s: AtomicU32,
}

impl RadioCoex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_wifi_busy(&self, busy: bool) {
        self.wifi_busy.store(busy, Ordering::SeqCst);
    }

    /// now から ms の間 BLE スキャンを止める (粒度は秒・切り上げ)。
    /// 既により長い停止が予約されている場合は短縮しない (延長のみ)
    pub fn pause_ble_for(&self, now_ms: u64, ms: u64) {
        let until_s = now_ms.saturating_add(ms).div_ceil(1000).min(u32::MAX as u64) as u32;
        self.ble_pause_until_s.fetch_max(until_s, Ordering::SeqCst);
    }

    /// BLE スキャンを今止めるべきか
    pub fn ble_should_pause(&self, now_ms: u64) -> bool {
        self.wifi_busy.load(Ordering::SeqCst)
            || (now_ms / 1000) < u64::from(self.ble_pause_until_s.load(Ordering::SeqCst))
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
        // 1.0s + 5.0s → 期限 6 秒 (切り上げ秒粒度)
        c.pause_ble_for(1_000, 5_000);
        assert!(c.ble_should_pause(5_999));
        assert!(!c.ble_should_pause(6_000));
    }

    #[test]
    fn later_shorter_pause_does_not_shrink() {
        let c = RadioCoex::new();
        c.pause_ble_for(1_000, 10_000); // 期限 11 秒
        c.pause_ble_for(2_000, 1_000); // 期限 3 秒 — 短いので無視
        assert!(c.ble_should_pause(10_999));
        assert!(!c.ble_should_pause(11_000));
    }

    #[test]
    fn saturates_at_u32_seconds() {
        let c = RadioCoex::new();
        c.pause_ble_for(u64::MAX, u64::MAX);
        assert!(c.ble_should_pause(u64::from(u32::MAX) * 1000 - 1_000));
    }
}
