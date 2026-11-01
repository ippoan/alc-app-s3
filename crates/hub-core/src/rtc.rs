//! BM8563 (CoreS3 の RTC、SE には無い) の時刻レジスタと Unix 秒 (UTC) の変換 (純粋ロジック)。
//!
//! 起動直後は SNTP が合うまで システム時刻が 1970 起点になり、その間の測定・打刻は
//! 1970 起点の時刻で送られる (回線が通ってから同期まで分単位かかることがある)。
//! RTC が持っている時刻で起動時に合わせ、SNTP で合ったら RTC へ書き戻す。
//!
//! - レジスタは 0x02 (秒) から 7 バイト: 秒 / 分 / 時 / 日 / 曜日 / 月 (bit7 = 世紀) / 年。値は BCD
//! - 秒の bit7 (VL) は電源が落ちて時刻が保証されない印。立っていたら読まない
//! - RTC には UTC を入れる。年は 2000〜2099 だけを扱い、世紀の bit は書かない (読むときも見ない)

use crate::clock::{civil_from_days, days_from_civil, MIN_SYNCED_SECS};

/// 時刻レジスタの先頭 (秒)
pub const TIME_REG: u8 = 0x02;
/// 時刻レジスタの長さ (秒・分・時・日・曜日・月・年)
pub const TIME_LEN: usize = 7;

/// 秒レジスタの VL (Voltage Low) — 時刻が保証されない
const VL: u8 = 0x80;
/// 扱う年の上限 (2 桁の BCD に収まる範囲)
const MAX_YEAR: i64 = 2099;

fn from_bcd(v: u8) -> Option<i64> {
    let (hi, lo) = (v >> 4, v & 0x0f);
    (hi < 10 && lo < 10).then_some(i64::from(hi) * 10 + i64::from(lo))
}

fn to_bcd(v: i64) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

/// 時刻レジスタを Unix 秒 (UTC) にする。VL が立っている・BCD でない・ありえない日付・
/// 未同期の範囲 (`MIN_SYNCED_SECS` 未満) なら None
pub fn decode(regs: [u8; TIME_LEN]) -> Option<i64> {
    if regs[0] & VL != 0 {
        return None;
    }
    let sec = from_bcd(regs[0] & 0x7f)?;
    let min = from_bcd(regs[1] & 0x7f)?;
    let hour = from_bcd(regs[2] & 0x3f)?;
    let day = from_bcd(regs[3] & 0x3f)?;
    let month = from_bcd(regs[5] & 0x1f)?;
    let year = 2000 + from_bcd(regs[6])?;
    if sec > 59 || min > 59 || hour > 23 || !(1..=12).contains(&month) || day < 1 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    // 2/30 などは日数に直すと別の日になる。往復して一致するものだけを受ける
    if civil_from_days(days) != (year, month, day) {
        return None;
    }
    let secs = days * 86_400 + hour * 3_600 + min * 60 + sec;
    (secs >= MIN_SYNCED_SECS).then_some(secs)
}

/// Unix 秒 (UTC) を時刻レジスタにする。未同期の範囲と 2100 年以降は None (書かない)
pub fn encode(unix_secs: i64) -> Option<[u8; TIME_LEN]> {
    if unix_secs < MIN_SYNCED_SECS {
        return None;
    }
    let days = unix_secs.div_euclid(86_400);
    let sod = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    if year > MAX_YEAR {
        return None;
    }
    // 1970-01-01 は木曜 (= 4、日曜 = 0)
    let weekday = (days + 4).rem_euclid(7);
    Some([
        to_bcd(sod % 60),
        to_bcd(sod % 3_600 / 60),
        to_bcd(sod / 3_600),
        to_bcd(day),
        to_bcd(weekday),
        to_bcd(month),
        to_bcd(year - 2000),
    ])
}

/// SNTP で合った時刻を RTC へ書くか。`synced_at` は直近の同期 (稼働時間 ms)、`written_at` は
/// 最後に書いた同期。同期のたびに 1 回だけ書く
pub fn should_write(synced_at: Option<u64>, written_at: Option<u64>) -> bool {
    synced_at.is_some() && synced_at != written_at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-01 06:53:57 UTC (日曜)
    const T: i64 = 1_793_516_037;
    const REGS: [u8; TIME_LEN] = [0x57, 0x53, 0x06, 0x01, 0x00, 0x11, 0x26];

    #[test]
    fn encode_and_decode_round_trip() {
        assert_eq!(encode(T), Some(REGS));
        assert_eq!(decode(REGS), Some(T));
        // 閏日と年末
        for t in [1_709_164_800, 1_767_225_599, MIN_SYNCED_SECS] {
            assert_eq!(decode(encode(t).unwrap()), Some(t), "{t}");
        }
    }

    #[test]
    fn decode_ignores_unused_bits() {
        // 分・時・日・月の上位の未使用ビットと世紀の bit は見ない
        let mut regs = REGS;
        regs[1] |= 0x80;
        regs[2] |= 0xc0;
        regs[3] |= 0xc0;
        regs[4] = 0xff;
        regs[5] |= 0xe0;
        assert_eq!(decode(regs), Some(T));
    }

    #[test]
    fn decode_rejects_untrusted_time() {
        let with = |i: usize, v: u8| {
            let mut regs = REGS;
            regs[i] = v;
            regs
        };
        // VL (電源が落ちた)
        assert_eq!(decode(with(0, REGS[0] | VL)), None);
        // BCD でない (上位・下位)
        assert_eq!(decode(with(0, 0x5a)), None);
        assert_eq!(decode(with(0, 0x7a & 0x7f)), None);
        assert_eq!(decode(with(1, 0x6a)), None);
        assert_eq!(decode(with(2, 0x0a)), None);
        assert_eq!(decode(with(3, 0x0a)), None);
        assert_eq!(decode(with(5, 0x0a)), None);
        assert_eq!(decode(with(6, 0xa0)), None);
        // 範囲外
        assert_eq!(decode(with(0, 0x60)), None);
        assert_eq!(decode(with(1, 0x60)), None);
        assert_eq!(decode(with(2, 0x24)), None);
        assert_eq!(decode(with(3, 0x00)), None);
        assert_eq!(decode(with(5, 0x00)), None);
        assert_eq!(decode(with(5, 0x13)), None);
        // ありえない日付 (2026-02-30)
        assert_eq!(decode([0, 0, 0, 0x30, 0, 0x02, 0x26]), None);
        // 書いたことのない RTC の典型 (2000-01-01) は未同期の範囲
        assert_eq!(decode([0, 0, 0, 0x01, 0, 0x01, 0x00]), None);
    }

    #[test]
    fn encode_refuses_unsynced_and_far_future() {
        assert_eq!(encode(0), None);
        assert_eq!(encode(MIN_SYNCED_SECS - 1), None);
        // 2100-01-01 00:00:00 UTC
        assert_eq!(encode(4_102_444_800), None);
        assert!(encode(4_102_444_799).is_some());
    }

    #[test]
    fn write_once_per_sync() {
        assert!(!should_write(None, None));
        assert!(should_write(Some(5), None));
        assert!(!should_write(Some(5), Some(5)));
        assert!(should_write(Some(9), Some(5)));
    }
}
