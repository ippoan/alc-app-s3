//! 直近 8 回の reset 理由の履歴 (純粋部分、ippoan/alc-app-s3#211)。
//!
//! 本番機で起きる再起動ループの原因切り分けには、直近 1 回の reset 理由では
//! 足りない — ループは USB を抜くしか止められず、抜いただけでも再起動するので、
//! 止めた時点の reset 理由は「抜いたことによる reset」に上書きされてしまう。
//! そこで直近 8 回分を NVS の `u64` 1 キーに詰めて持ち回す。
//!
//! 1 件 8 ビット、**新しいものが下位バイト**。空 (未記録) は `0xFF`、初期値は
//! 全バイトが `0xFF` の [`EMPTY`]。8 件を超えると最古の 1 件が自然に押し出される。

/// 履歴が空 (1 件も記録されていない) ときの詰め込み値。全バイトが `0xFF`。
pub const EMPTY: u64 = u64::MAX;

/// `packed` の末尾に reset code を 1 件積む。新しい値を返す。
///
/// `code` は 1 バイトに収める都合上 `0..=254` へ clamp する (`0xFF` は空きの
/// 目印として予約)。8 件を超えた最古の 1 件は上位バイトから自然に押し出される。
pub fn push(packed: u64, code: i32) -> u64 {
    let byte = code.clamp(0, 254) as u64;
    (packed << 8) | byte
}

/// `packed` から reset code を新しい順に取り出す。最大 8 件、`0xFF` (空き) に
/// 当たったらそこで打ち切る。
pub fn codes(packed: u64) -> Vec<i32> {
    let mut out = Vec::with_capacity(8);
    for i in 0..8 {
        let byte = (packed >> (i * 8)) & 0xFF;
        if byte == 0xFF {
            break;
        }
        out.push(byte as i32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_yields_no_codes() {
        assert_eq!(codes(EMPTY), Vec::<i32>::new());
    }

    #[test]
    fn single_push() {
        let packed = push(EMPTY, 11);
        assert_eq!(codes(packed), vec![11]);
    }

    #[test]
    fn exactly_eight_entries_all_kept_newest_first() {
        let mut packed = EMPTY;
        for code in 1..=8 {
            packed = push(packed, code);
        }
        assert_eq!(codes(packed), vec![8, 7, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn ninth_entry_evicts_oldest() {
        let mut packed = EMPTY;
        for code in 1..=9 {
            packed = push(packed, code);
        }
        assert_eq!(codes(packed), vec![9, 8, 7, 6, 5, 4, 3, 2]);
    }

    #[test]
    fn mid_history_ff_stops_enumeration() {
        // 空 (EMPTY) から 3 件だけ積んだ状態を模す: 上位側は 0xFF のまま残る
        let packed = push(push(push(EMPTY, 3), 2), 1);
        assert_eq!(codes(packed), vec![1, 2, 3]);
    }

    #[test]
    fn clamps_negative_and_overflowing_codes() {
        assert_eq!(codes(push(EMPTY, -1)), vec![0]);
        assert_eq!(codes(push(EMPTY, 255)), vec![254]);
        assert_eq!(codes(push(EMPTY, 1000)), vec![254]);
    }
}
