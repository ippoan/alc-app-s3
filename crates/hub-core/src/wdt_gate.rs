//! Task WDT 購読状態の純粋ロジック (Refs #55)。
//!
//! UI ループ (メインタスク) を Task WDT で監視するが、**OTA のような長時間
//! CPU 専有処理中は 10s 以上 feed できず誤リセットする**ため、その間だけ監視を
//! 外す必要がある (実害: #55 — OTA が毎回 task_wdt reset で中断)。
//!
//! 「今 UI タスクの WDT を購読すべきか」の判断を、ネスト・不均衡呼び出しでも
//! 壊れない深さカウンタとしてここに純粋化し、host で単体テスト・カバレッジ 100%
//! を強制する。実際の `esp_task_wdt_add/delete` 呼び出しは hub-common::wdt
//! (FFI 層) が、本 gate が返す**遷移**に応じて行う。

/// pause の深さで WDT 監視状態を管理する。`pause`/`resume` は「監視⇄停止の
/// 遷移が起きたか」を返し、FFI 層はその時だけ実際の add/delete を呼ぶ。
#[derive(Debug, Default, Clone, Copy)]
pub struct WdtGate {
    /// pause の深さ (0 = 監視中)。resume で減る。
    pause_depth: u32,
}

impl WdtGate {
    pub const fn new() -> Self {
        Self { pause_depth: 0 }
    }

    /// 監視中か (depth == 0)。
    pub fn is_watching(&self) -> bool {
        self.pause_depth == 0
    }

    /// 現在の pause 深さ。
    pub fn depth(&self) -> u32 {
        self.pause_depth
    }

    /// 一時停止する。**監視中→停止に遷移した時だけ** `true` を返す
    /// (= FFI 層が実際に unsubscribe すべきタイミング)。ネストした 2 回目以降は
    /// 深さだけ増やして `false`。飽和加算で overflow しない。
    pub fn pause(&mut self) -> bool {
        let was_watching = self.pause_depth == 0;
        self.pause_depth = self.pause_depth.saturating_add(1);
        was_watching
    }

    /// 再開する。**停止→監視に遷移した時だけ** `true` を返す
    /// (= FFI 層が実際に re-subscribe すべきタイミング)。
    /// 既に監視中 (不均衡な resume) なら深さ 0 のまま `false` を返す (fail-safe:
    /// 余分な resume で監視を二重登録しない)。
    pub fn resume(&mut self) -> bool {
        if self.pause_depth == 0 {
            return false;
        }
        self.pause_depth -= 1;
        self.pause_depth == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_watching() {
        let g = WdtGate::new();
        assert!(g.is_watching());
        assert_eq!(g.depth(), 0);
    }

    #[test]
    fn pause_then_resume_transitions_once_each() {
        let mut g = WdtGate::new();
        // 監視中→停止の遷移で true
        assert!(g.pause());
        assert!(!g.is_watching());
        // 停止→監視の遷移で true
        assert!(g.resume());
        assert!(g.is_watching());
    }

    #[test]
    fn nested_pause_only_first_transitions() {
        let mut g = WdtGate::new();
        assert!(g.pause()); // 0→1: 遷移
        assert!(!g.pause()); // 1→2: 遷移なし
        assert!(!g.pause()); // 2→3: 遷移なし
        assert_eq!(g.depth(), 3);
        assert!(!g.is_watching());
        // 対応する resume: 最後の 1 回だけ監視へ戻る
        assert!(!g.resume()); // 3→2
        assert!(!g.resume()); // 2→1
        assert!(!g.is_watching());
        assert!(g.resume()); // 1→0: 遷移
        assert!(g.is_watching());
    }

    #[test]
    fn unbalanced_resume_is_noop_and_stays_watching() {
        let mut g = WdtGate::new();
        // 監視中の resume は何もしない (二重登録防止)
        assert!(!g.resume());
        assert!(g.is_watching());
        assert_eq!(g.depth(), 0);
        // 直後の pause は通常どおり遷移する
        assert!(g.pause());
        assert!(!g.is_watching());
    }

    #[test]
    fn pause_saturates_without_overflow() {
        let mut g = WdtGate::new();
        g.pause(); // 0→1 (遷移)
        for _ in 0..5 {
            assert!(!g.pause());
        }
        // 過剰 resume は 0 で止まり、以降 false
        for _ in 0..3 {
            g.resume();
        }
        // depth は 6-3=3 のはず
        assert_eq!(g.depth(), 3);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(WdtGate::default().depth(), WdtGate::new().depth());
    }
}
