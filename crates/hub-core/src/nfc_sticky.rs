//! Type-B (免許証) 読み取りの **B 粘着** の状態遷移 (issue #155)。
//!
//! `PollOrder::LicenseFirst` (hub-drivers の nfc.rs) は、待機中 B モードのまま電界 ON に
//! しておき、トリガ後は切替 (電界断) ゼロで B から読む。免許証 (Type-B、暗号コプロ付き) は
//! F → A → B の各先頭で入る電界断 (10ms × 3/周) の直後には電源が立ち上がりきらず、
//! WUPB に応答できないまま痕跡ゼロで落ちる — 1 周の 3 回の電源断が「かざしても
//! 反応しない」の読み取り側の原因だった (実機 4 build、2026-09-06)。
//!
//! B が応答した (成功以外の途中死も含む) 周の次は、F/A を飛ばして **B だけを電界断なしで
//! 再試行**する = 粘着。実機では粘着中の B が 84ms ごとに応答し、途中死しても次周で
//! 読了する。粘着を解く条件は 3 つ:
//!
//! - **読了** (rc = 0)
//! - **無応答 (-2) が [`RELEASE_MISSES`] 周連続** = カードが離れた
//! - **上限 [`MAX_CYCLES`] 周** = 途中死 (-3 / -5 / -6) が続く限り粘ると F/A が周回から
//!   閉め出されるので打ち切る (免許証の読了 5〜6 周の 2 倍)
//! - **SELECT MF 失敗 (-4)** = 免許証でない Type-B が載っている。その周で解く。
//!   **スマホ (モバイル Suica) は HCE で Type-B にも応答する**ので、ここで粘ると
//!   F (FeliCa) が閉め出されて Suica が読めない/遅くなる (実機: 3 周粘ると
//!   スマホの応答が 1.0 秒、旧 F 先行は 0.35 秒)。弱結合の免許証でも -4 は出るが稀
//!   (実機 27 タップ中 2) で、-4 は ATTRIB が通った後なので結合は既にあり、F/A を
//!   1 周挟んだ次の B で読める
//!
//! 純関数にしてホストでテストする (`nfc_tap` と同じ流儀)。**rc の意味は nfc_shim の
//! `nfc_shim_read_license_expiry()` の戻り値**: 0 読了 / -1 未初期化 / -2 無応答 /
//! -3 ATTRIB 失敗 / -4 SELECT MF 失敗 / -5 SELECT EF 失敗 / -6 READ BINARY 失敗。

/// 粘着を解く、無応答 (-2) の連続周回数
pub const RELEASE_MISSES: u32 = 2;
/// 粘着の連続周回数の上限
pub const MAX_CYCLES: u32 = 12;

/// nfc_shim の rc: カード無し (WUPB 無応答)
pub const RC_NO_CARD: i32 = -2;
/// nfc_shim の rc: 未初期化 or バッファ不足。無応答と同じ扱い
pub const RC_NOT_READY: i32 = -1;
/// nfc_shim の rc: SELECT MF 失敗 (免許証以外の Type-B の可能性)
pub const RC_SELECT_MF_FAILED: i32 = -4;

/// 粘着の状態。`Default` = 粘着していない
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sticky {
    /// 粘着中 (次周は F/A を飛ばして B だけ)
    pub on: bool,
    /// 粘着中の無応答 (-2) の連続周回数
    pub misses: u32,
    /// 粘着の連続周回数
    pub cycles: u32,
}

/// この周で粘着が解けた理由 (計器用)。解けていなければ [`Release::None`]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    None,
    /// 読了
    Read,
    /// 無応答が [`RELEASE_MISSES`] 周連続
    Misses,
    /// [`MAX_CYCLES`] 周を超えた
    MaxCycles,
    /// SELECT MF 失敗 (免許証でない Type-B)
    MfFail,
}

impl Release {
    /// 計器行の `sticky=` に出す語。解けていなければ粘着中かどうか
    pub fn label(self, on: bool) -> &'static str {
        match self {
            Release::None if on => "true",
            Release::None => "false",
            Release::Read => "released(read)",
            Release::Misses => "released(miss)",
            Release::MaxCycles => "released(max)",
            Release::MfFail => "released(mf)",
        }
    }
}

/// B の結果 `rc` を受けて次の粘着状態を返す。毎周 (B を打った周) に 1 回呼ぶ
pub fn next(prev: Sticky, rc: i32) -> (Sticky, Release) {
    let mut s = prev;
    match rc {
        0 => return (Sticky::default(), Release::Read),
        RC_NO_CARD | RC_NOT_READY => {
            if !s.on {
                return (s, Release::None);
            }
            s.misses += 1;
            if s.misses >= RELEASE_MISSES {
                return (Sticky::default(), Release::Misses);
            }
        }
        RC_SELECT_MF_FAILED => return (Sticky::default(), Release::MfFail),
        _ => {
            // 免許証として応答した後の途中死 (-3 ATTRIB / -5 SELECT EF / -6 READ BINARY)。
            // 次周も B だけを電界断なしで再試行する
            s.on = true;
            s.misses = 0;
        }
    }
    s.cycles += 1;
    if s.cycles > MAX_CYCLES {
        return (Sticky::default(), Release::MaxCycles);
    }
    (s, Release::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(rcs: &[i32]) -> (Sticky, Release) {
        let mut s = Sticky::default();
        let mut r = Release::None;
        for &rc in rcs {
            (s, r) = next(s, rc);
        }
        (s, r)
    }

    #[test]
    fn no_card_while_idle_stays_idle() {
        let (s, r) = run(&[-2, -2, -1]);
        assert_eq!(s, Sticky::default());
        assert_eq!(r, Release::None);
    }

    #[test]
    fn partial_response_starts_sticky() {
        let (s, r) = run(&[-3]);
        assert!(s.on);
        assert_eq!((s.misses, s.cycles), (0, 1));
        assert_eq!(r, Release::None);
    }

    #[test]
    fn read_releases() {
        let (s, r) = run(&[-3, -5, 0]);
        assert_eq!(s, Sticky::default());
        assert_eq!(r, Release::Read);
    }

    #[test]
    fn one_miss_keeps_sticky_two_misses_release() {
        let (s, r) = run(&[-6, -2]);
        assert!(s.on);
        assert_eq!((s.misses, s.cycles), (1, 2));
        assert_eq!(r, Release::None);
        let (s, r) = run(&[-6, -2, -1]);
        assert_eq!(s, Sticky::default());
        assert_eq!(r, Release::Misses);
    }

    #[test]
    fn response_after_a_miss_resets_misses() {
        let (s, r) = run(&[-3, -2, -5]);
        assert!(s.on);
        assert_eq!((s.misses, s.cycles), (0, 3));
        assert_eq!(r, Release::None);
    }

    #[test]
    fn max_cycles_releases() {
        let seq = vec![-3; MAX_CYCLES as usize];
        let (s, r) = run(&seq);
        assert!(s.on);
        assert_eq!(s.cycles, MAX_CYCLES);
        assert_eq!(r, Release::None);
        let seq = vec![-3; MAX_CYCLES as usize + 1];
        let (s, r) = run(&seq);
        assert_eq!(s, Sticky::default());
        assert_eq!(r, Release::MaxCycles);
    }

    #[test]
    fn select_mf_failure_releases_next_cycle() {
        // スマホ (HCE) の Type-B 応答: 初回の -4 で解いて同周内に F/A へ
        let (s, r) = run(&[-4]);
        assert_eq!(s, Sticky::default());
        assert_eq!(r, Release::MfFail);
    }

    #[test]
    fn select_mf_failure_releases_even_while_sticky() {
        // 弱結合の免許証が -4 を返した周も解く (F/A を 1 周挟んで次の B で読める)
        let (s, r) = run(&[-3, -5, -4]);
        assert_eq!(s, Sticky::default());
        assert_eq!(r, Release::MfFail);
    }

    #[test]
    fn labels() {
        assert_eq!(Release::None.label(true), "true");
        assert_eq!(Release::None.label(false), "false");
        assert_eq!(Release::Read.label(false), "released(read)");
        assert_eq!(Release::Misses.label(false), "released(miss)");
        assert_eq!(Release::MaxCycles.label(false), "released(max)");
        assert_eq!(Release::MfFail.label(false), "released(mf)");
    }
}
