//! バイタル (体温/血圧) 表示・イベントログ用の文字列整形。

/// 体温の表示値 "36.8"
pub fn temp_value(celsius: f32) -> String {
    format!("{celsius:.1}")
}

/// 血圧の表示値 "120/80"
pub fn bp_value(systolic: f32, diastolic: f32) -> String {
    format!("{systolic:.0}/{diastolic:.0}")
}

/// 脈拍の表示行 "脈拍 72"
pub fn pulse_value(pulse: f32) -> String {
    format!("脈拍 {pulse:.0}")
}

/// イベントログ行 "体温 36.8℃"
pub fn temp_event(celsius: f32) -> String {
    format!("体温 {celsius:.1}℃")
}

/// イベントログ行 "血圧 120/80 脈拍72"
pub fn bp_event(systolic: f32, diastolic: f32, pulse: Option<f32>) -> String {
    match pulse {
        Some(p) if p > 0.0 => format!("血圧 {systolic:.0}/{diastolic:.0} 脈拍{p:.0}"),
        _ => format!("血圧 {systolic:.0}/{diastolic:.0}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(temp_value(36.75), "36.8");
        assert_eq!(bp_value(120.4, 79.6), "120/80");
        assert_eq!(pulse_value(72.3), "脈拍 72");
    }

    #[test]
    fn temp_event_format() {
        assert_eq!(temp_event(36.8), "体温 36.8℃");
    }

    #[test]
    fn bp_event_with_pulse() {
        assert_eq!(bp_event(120.0, 80.0, Some(72.0)), "血圧 120/80 脈拍72");
    }

    #[test]
    fn bp_event_without_pulse() {
        assert_eq!(bp_event(120.0, 80.0, None), "血圧 120/80");
        // 0 以下の脈拍は無効値として表示しない
        assert_eq!(bp_event(120.0, 80.0, Some(0.0)), "血圧 120/80");
    }
}
