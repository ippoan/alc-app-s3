//! 点呼セッション識別子の採番 (Refs #112)。
//!
//! 1 回の点呼 (UI の `Measuring` に入ってから待機画面へ戻るまで) で採れた
//! 体温・血圧・アルコールを、サーバ側で 1 グループとして扱えるようにするための
//! ID を発番する。これが無いと「どの測定が同じ点呼のものか」は受信時刻の近接から
//! 推測するしかなく、2 人が続けて測ると混ざる / seq に欠番があると判断できない。
//!
//! ## 一意性の範囲
//!
//! 値は **この端末の中でのみ一意**。サーバ側の一意性は
//! `(tenant_id, device_id, session_id)` の組で担保される (rust-alc-api の
//! migration 127 参照) ため、他端末との衝突は考えなくてよい。
//!
//! ## なぜ boot_id を混ぜるか
//!
//! 単なる連番だと再起動でカウンタが 0 に戻り、**再起動前のセッションと同じ ID** が
//! 再利用される。サーバ側から見ると別々の点呼が 1 つに融合して見えてしまうため、
//! NVS 永続の起動カウンタ (boot_id) を前置して「起動をまたいでも衝突しない」形にする。
//! 時刻を使わないのは、NTP 未同期の端末では epoch が 1970 起点の稼働時間になり
//! 再起動のたびに巻き戻るため (それでは衝突を防げない)。

/// session_id の長さ上限。rust-alc-api の `MAX_SESSION_ID_LEN` と一致させる
/// (超えると ingest が 400 で弾く)。
pub const MAX_SESSION_ID_LEN: usize = 64;

/// 1 回の点呼ごとに ID を発番する。
///
/// UI スレッドが 1 つだけ保持する (点呼の開始を知っているのは UI のため)。
/// recorder は発番せず、`HubStatus` に載った現在値を読むだけ。
#[derive(Debug, Clone)]
pub struct SessionIdGen {
    boot_id: u32,
    counter: u32,
}

impl SessionIdGen {
    /// `boot_id` は NVS 永続の起動カウンタ (起動ごとに +1 された値)。
    pub fn new(boot_id: u32) -> Self {
        Self {
            boot_id,
            counter: 0,
        }
    }

    /// 次の session_id を発番する。`{boot_id}-{連番}` 形式。
    ///
    /// 連番は 1 起点 (0 起点にすると「未発番」と見分けにくいログになる)。
    /// u32 の飽和は実質起こらない (1 起動で 40 億回の点呼) が、万一到達しても
    /// wrapping させず飽和させる — 同じ ID を再利用して点呼が融合するより、
    /// 同じ ID が続いた方が異常として気づきやすい。
    pub fn next(&mut self) -> String {
        self.counter = self.counter.saturating_add(1);
        format!("{}-{}", self.boot_id, self.counter)
    }
}

/// session_id として受理できる形か (rust-alc-api の `valid_session_id` と同条件)。
///
/// 端末が発番した値をそのまま送る経路なので通常は必ず真だが、`AUTH`/`CFG` 系の
/// 手動注入や将来の形式変更で不正値が混じったときに、送信前に気づけるようにする。
pub fn is_valid_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_SESSION_ID_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_is_monotonic_within_a_boot() {
        let mut gen = SessionIdGen::new(7);
        assert_eq!(gen.next(), "7-1");
        assert_eq!(gen.next(), "7-2");
        assert_eq!(gen.next(), "7-3");
    }

    #[test]
    fn different_boots_never_collide() {
        // 再起動で連番は 1 に戻るが、boot_id が違うので ID は衝突しない
        let mut first = SessionIdGen::new(1);
        let mut second = SessionIdGen::new(2);
        assert_ne!(first.next(), second.next());
    }

    #[test]
    fn counter_saturates_instead_of_wrapping() {
        // 巻き戻して同じ ID を再利用する (= 別々の点呼が融合する) ことは避ける
        let mut gen = SessionIdGen {
            boot_id: 1,
            counter: u32::MAX - 1,
        };
        assert_eq!(gen.next(), format!("1-{}", u32::MAX));
        assert_eq!(gen.next(), format!("1-{}", u32::MAX));
    }

    #[test]
    fn generated_ids_pass_server_side_validation() {
        let mut gen = SessionIdGen::new(u32::MAX);
        for _ in 0..3 {
            let id = gen.next();
            assert!(is_valid_session_id(&id), "id={id}");
        }
    }

    #[test]
    fn validation_rejects_empty_long_and_bad_charset() {
        assert!(is_valid_session_id("s-42_7"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id(&"x".repeat(MAX_SESSION_ID_LEN + 1)));
        assert!(is_valid_session_id(&"x".repeat(MAX_SESSION_ID_LEN)));
        assert!(!is_valid_session_id("bad id"));
        assert!(!is_valid_session_id("a/b"));
        assert!(!is_valid_session_id("日本語"));
    }
}
