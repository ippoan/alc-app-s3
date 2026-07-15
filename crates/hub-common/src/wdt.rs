//! Task WDT の購読管理 (Refs #50, #55)。
//!
//! UI ループ (メインタスク) を Task WDT で監視して wedge から自動復帰するが、
//! **OTA のような長時間 CPU 専有処理の間は UI タスクが 10s 以上 feed できず
//! 誤リセットする** (実害: #55 — OTA 中に task_wdt reset で更新が毎回中断)。
//!
//! そこで UI タスクのハンドルを保持し、OTA など長時間処理の前後で
//! **別タスクからでも** UI タスクの WDT 購読を一時解除/再登録できるようにする。
//! `esp_task_wdt_delete/add` はタスクハンドル指定でき、呼び出し元タスクを問わない。

use core::sync::atomic::{AtomicUsize, Ordering};

use esp_idf_svc::sys;

/// UI ループ (メインタスク) の TaskHandle を usize で保持 (0 = 未登録)。
static UI_TASK: AtomicUsize = AtomicUsize::new(0);

/// 現在タスク (= UI ループ) を Task WDT に登録し、ハンドルを保存する。
/// 起動時に UI ループから 1 回だけ呼ぶ。失敗しても運転は継続 (fail-open)。
pub fn subscribe_current_as_ui() {
    unsafe {
        let h = sys::xTaskGetCurrentTaskHandle();
        UI_TASK.store(h as usize, Ordering::SeqCst);
        let _ = sys::esp_task_wdt_add(h);
    }
}

/// 現在タスク (UI ループ) の WDT を feed する。ループ毎に呼ぶ。
pub fn feed() {
    unsafe {
        let _ = sys::esp_task_wdt_reset();
    }
}

/// UI タスクの WDT 監視を一時解除する (OTA 等の長時間処理の直前に呼ぶ)。
/// 別タスク (OTA スレッド) から呼んでよい。未登録なら何もしない。
pub fn pause_ui() {
    let h = UI_TASK.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = sys::esp_task_wdt_delete(h as sys::TaskHandle_t);
        }
    }
}

/// `pause_ui()` で解除した UI タスクの WDT 監視を戻す (長時間処理の完了後)。
pub fn resume_ui() {
    let h = UI_TASK.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = sys::esp_task_wdt_add(h as sys::TaskHandle_t);
        }
    }
}
