//! Unix 時刻 → 日本時間 (JST) の表示文字列への変換 (純粋ロジック)。
//!
//! NTP (SNTP) 同期後の壁時計時刻を、ログ用に "MM/DD HH:MM:SS" (JST) へ
//! 整形する。同期前 (1970 起点の小さい値) は None を返し、呼び出し側が
//! 稼働時間表示へフォールバックする。

/// これ未満の Unix 秒は「時刻未同期」とみなす (2020-09 頃)
const MIN_SYNCED_SECS: i64 = 1_600_000_000;

/// Unix 秒 (UTC) を JST の "MM/DD HH:MM:SS" に整形する。未同期なら None。
pub fn format_jst(unix_secs: i64) -> Option<String> {
    if unix_secs < MIN_SYNCED_SECS {
        return None;
    }
    let t = unix_secs + 9 * 3600; // JST = UTC+9
    let days = t.div_euclid(86400);
    let sod = t.rem_euclid(86400); // 0..86399
    let (_y, m, d) = civil_from_days(days);
    let h = sod / 3600;
    let mi = (sod % 3600) / 60;
    let s = sod % 60;
    Some(format!("{m:02}/{d:02} {h:02}:{mi:02}:{s:02}"))
}

/// 1970-01-01 からの日数 → (年, 月, 日)。Howard Hinnant の civil_from_days。
/// div_euclid を使い負値でも分岐なしで floor 除算する。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsynced_returns_none() {
        assert_eq!(format_jst(0), None);
        assert_eq!(format_jst(MIN_SYNCED_SECS - 1), None);
    }

    #[test]
    fn new_year_2026_jst() {
        // 1767225600 = 2026-01-01 00:00:00 UTC → JST 2026-01-01 09:00:00
        assert_eq!(format_jst(1_767_225_600).as_deref(), Some("01/01 09:00:00"));
    }

    #[test]
    fn summer_date_jst() {
        // 1782864000 = 2026-07-01 00:00:00 UTC → JST 2026-07-01 09:00:00
        assert_eq!(format_jst(1_782_864_000).as_deref(), Some("07/01 09:00:00"));
    }

    #[test]
    fn crosses_date_by_jst_offset() {
        // 1767222000 = 2025-12-31 23:00:00 UTC → +9h → 2026-01-01 08:00:00
        assert_eq!(format_jst(1_767_222_000).as_deref(), Some("01/01 08:00:00"));
    }

    #[test]
    fn boundary_is_synced_with_seconds() {
        // ちょうど閾値 (2020-09-13 12:26:40 UTC → JST 21:26:40) は同期扱い
        assert_eq!(format_jst(MIN_SYNCED_SECS).as_deref(), Some("09/13 21:26:40"));
    }
}
