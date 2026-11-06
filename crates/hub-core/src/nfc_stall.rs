//! NFC の読み取りループが止まったかの判定 (純粋ロジック、Refs #352)。
//!
//! 読み取りループ (`alc_hub_drivers::nfc`) は 1 周ごとに時刻を残す。見張りのスレッドがそれを
//! 1 秒ごとに見て、[`STALL_MS`] 進んでいなければ止まったとみなす (`EVT NFC_STALL` を残して再起動)。
//! ループは 1 周およそ 0.3 秒で、同期の読み取りが一番長く掛かる免許証の APDU でも 2 秒ほどなので、
//! 30 秒は正常な周回では届かない。
//!
//! ホスト (キオスク) には [`ALIVE_EVERY_MS`] ごとに `EVT NFC_ALIVE` を出す。画面はそれが
//! 途絶えたら「NFC 応答なし」にする (画面側の閾値はこの間隔の数倍)。

/// この時間ループが進まなければ止まったとみなす (ms)
pub const STALL_MS: u64 = 30_000;

/// `EVT NFC_ALIVE` を出す間隔 (ms)
pub const ALIVE_EVERY_MS: u64 = 10_000;

/// ループが止まっているなら、最後に周回してからの経過 (ms) を返す。
/// `last_loop_ms == 0` はまだ待受に入っていない (初期化の再試行中を含む) ので見ない
pub fn stalled_for(now_ms: u64, last_loop_ms: u64) -> Option<u64> {
    if last_loop_ms == 0 {
        return None;
    }
    let gap = now_ms.saturating_sub(last_loop_ms);
    (gap >= STALL_MS).then_some(gap)
}

/// `EVT NFC_ALIVE` を出す時刻か (`last_alive_ms == 0` は未送 = すぐ出す)
pub fn alive_due(now_ms: u64, last_alive_ms: u64) -> bool {
    last_alive_ms == 0 || now_ms.saturating_sub(last_alive_ms) >= ALIVE_EVERY_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_started_is_never_stalled() {
        assert_eq!(stalled_for(1_000_000, 0), None);
    }

    #[test]
    fn stalled_only_after_threshold() {
        let last = 100_000;
        assert_eq!(stalled_for(last + STALL_MS - 1, last), None);
        assert_eq!(stalled_for(last + STALL_MS, last), Some(STALL_MS));
        assert_eq!(stalled_for(last + 45_000, last), Some(45_000));
    }

    #[test]
    fn clock_behind_last_loop_is_not_stalled() {
        // 見張りが読んだ時刻より後にループが時刻を書いた (競合) ときは 0 とみなす
        assert_eq!(stalled_for(99, 100), None);
    }

    #[test]
    fn alive_first_then_every_interval() {
        assert!(alive_due(5, 0));
        assert!(!alive_due(1_000 + ALIVE_EVERY_MS - 1, 1_000));
        assert!(alive_due(1_000 + ALIVE_EVERY_MS, 1_000));
    }
}
