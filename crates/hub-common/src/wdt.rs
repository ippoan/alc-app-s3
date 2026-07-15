//! Task WDT の購読管理 — FFI 層 (Refs #50, #55)。
//!
//! UI ループ (メインタスク) を Task WDT で監視して wedge から自動復帰するが、
//! **OTA のような長時間 CPU 専有処理の間は UI タスクが 10s 以上 feed できず
//! 誤リセットする** (実害: #55 — OTA 中に task_wdt reset で更新が毎回中断)。
//!
//! そこで UI タスクのハンドルを保持し、OTA など長時間処理の前後で
//! **別タスクからでも** UI タスクの WDT 購読を一時解除/再登録できるようにする。
//! `esp_task_wdt_delete/add` はタスクハンドル指定でき、呼び出し元タスクを問わない。
//!
//! pause/resume の**判断ロジック**は `alc_hub_core::wdt_gate::WdtGate`
//! (純粋・テスト済み・coverage 100%) に委ね、ここは「遷移が起きた時だけ実際の
//! esp_task_wdt を叩く」FFI に徹する。長時間処理側は `OtaWdtPause` ガードを使えば
//! panic / 早期 return でも resume を落とさない。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use alc_hub_core::wdt_gate::WdtGate;
use esp_idf_svc::sys;

/// UI ループ (メインタスク) の TaskHandle を usize で保持 (0 = 未登録)。
static UI_TASK: AtomicUsize = AtomicUsize::new(0);

/// pause/resume の状態機械 (ネスト・不均衡呼び出しに耐える)。
static GATE: Mutex<WdtGate> = Mutex::new(WdtGate::new());

/// 毒化しても状態は保持したい (into_inner) — WDT の一時停止判断が飛ぶと
/// OTA 中の誤リセットに戻るため、ロック毒化では倒れない。
fn gate() -> std::sync::MutexGuard<'static, WdtGate> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn ui_handle() -> Option<sys::TaskHandle_t> {
    let h = UI_TASK.load(Ordering::SeqCst);
    (h != 0).then_some(h as sys::TaskHandle_t)
}

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

/// UI タスクの WDT 監視を一時解除する (OTA 等の長時間処理の直前)。
/// 別タスク (OTA スレッド) から呼んでよい。ネストしても最初の 1 回だけ実解除する。
/// 直接呼ぶより `OtaWdtPause` ガード推奨 (resume 忘れ防止)。
pub fn pause_ui() {
    let transitioned = gate().pause();
    if transitioned {
        if let Some(h) = ui_handle() {
            unsafe {
                let _ = sys::esp_task_wdt_delete(h);
            }
        }
    }
}

/// `pause_ui()` で解除した監視を戻す。対応する pause と同数呼んだ時だけ実再登録する。
pub fn resume_ui() {
    let transitioned = gate().resume();
    if transitioned {
        if let Some(h) = ui_handle() {
            unsafe {
                let _ = sys::esp_task_wdt_add(h);
            }
        }
    }
}

/// OTA 等の長時間処理を囲む RAII ガード。生成で `pause_ui()`、drop で `resume_ui()`。
/// 途中で panic / 早期 return しても drop で必ず監視が戻るため、resume 忘れが
/// 構造的に起きない (#55 の回帰防止)。
#[must_use = "drop された時点で WDT 監視が戻るため、束縛して生存させること"]
pub struct OtaWdtPause;

impl OtaWdtPause {
    pub fn new() -> Self {
        pause_ui();
        OtaWdtPause
    }
}

impl Default for OtaWdtPause {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OtaWdtPause {
    fn drop(&mut self) {
        resume_ui();
    }
}
