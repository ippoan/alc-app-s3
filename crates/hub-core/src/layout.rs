//! 画面レイアウトの純粋計算。

/// QR モジュールの拡大倍率。利用可能ピクセルに収まるよう計算し、
/// 視認性のため 2..=8 に制限する (+2 はクワイエットゾーン相当の余白)。
pub fn qr_scale(avail_px: i32, modules: i32) -> i32 {
    (avail_px / (modules + 2)).clamp(2, 8)
}

/// 稼働時間の "HH:MM:SS" 表記
pub fn fmt_uptime(now_ms: u64) -> String {
    let s = now_ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// 指定文字数で折り返す (2 倍拡大描画の幅制約用。全角想定の単純な文字数分割)
pub fn wrap_chars(s: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![s.to_string()];
    }
    let chars: Vec<char> = s.chars().collect();
    chars
        .chunks(max_chars)
        .map(|c| c.iter().collect())
        .collect()
}

/// タッチ座標 (パネルネイティブ, 回転 0 基準) を現在の画面回転の論理座標へ変換。
/// 90/270 の回転方向は mipidsi の Rotation との対応で決めており、実機で要確認。
pub fn map_touch(x: i32, y: i32, rotation: u16, native_w: i32, native_h: i32) -> (i32, i32) {
    match rotation {
        90 => (y, native_w - 1 - x),
        180 => (native_w - 1 - x, native_h - 1 - y),
        270 => (native_h - 1 - y, x),
        _ => (x, y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_scale_typical() {
        // 25 モジュール (Version 2) を 196px に収める → 196/27 = 7
        assert_eq!(qr_scale(196, 25), 7);
    }

    #[test]
    fn qr_scale_clamped_low() {
        assert_eq!(qr_scale(50, 100), 2);
    }

    #[test]
    fn qr_scale_clamped_high() {
        assert_eq!(qr_scale(10_000, 21), 8);
    }

    #[test]
    fn fmt_uptime_zero() {
        assert_eq!(fmt_uptime(0), "00:00:00");
    }

    #[test]
    fn fmt_uptime_hms() {
        // 1時間1分1秒
        assert_eq!(fmt_uptime(3_661_000), "01:01:01");
    }

    #[test]
    fn fmt_uptime_rolls_minutes_not_hours() {
        assert_eq!(fmt_uptime(59_000), "00:00:59");
        assert_eq!(fmt_uptime(60_000), "00:01:00");
    }

    #[test]
    fn wrap_chars_splits_by_count() {
        assert_eq!(
            wrap_chars("カードをかざしてください", 8),
            vec!["カードをかざして".to_string(), "ください".to_string()]
        );
    }

    #[test]
    fn wrap_chars_short_and_empty() {
        assert_eq!(wrap_chars("短い", 8), vec!["短い".to_string()]);
        assert_eq!(wrap_chars("", 8), Vec::<String>::new());
    }

    #[test]
    fn wrap_chars_zero_max_returns_whole() {
        assert_eq!(wrap_chars("abc", 0), vec!["abc".to_string()]);
    }

    #[test]
    fn map_touch_all_rotations() {
        // 320x240 パネルの (10, 20)
        assert_eq!(map_touch(10, 20, 0, 320, 240), (10, 20));
        assert_eq!(map_touch(10, 20, 90, 320, 240), (20, 309));
        assert_eq!(map_touch(10, 20, 180, 320, 240), (309, 219));
        assert_eq!(map_touch(10, 20, 270, 320, 240), (219, 10));
    }
}
