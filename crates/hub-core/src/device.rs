//! 対象 BLE 機器 (ニプロ体温計/血圧計) の種別と判定。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Thermometer,
    BloodPressure,
}

impl DeviceKind {
    /// ble-medical-gateway のシリアル JSON 互換のデバイス名
    pub fn json_name(self) -> &'static str {
        match self {
            Self::Thermometer => "thermometer",
            Self::BloodPressure => "blood_pressure",
        }
    }

    /// 画面・イベントログ用の日本語名
    pub fn jp_name(self) -> &'static str {
        match self {
            Self::Thermometer => "体温計",
            Self::BloodPressure => "血圧計",
        }
    }
}

/// アドバタイズのデバイス名から種別を判定する (Arduino 版の名前判定を移植。
/// ニプロ機器が標準サービス UUID を広告しない場合の対策)。
pub fn match_device_name(name: &str) -> Option<DeviceKind> {
    if name.contains("NT-100") || name.contains("Thermo") {
        return Some(DeviceKind::Thermometer);
    }
    if name.contains("NBP-1") || name.contains("BP") || name.contains("Blood") {
        return Some(DeviceKind::BloodPressure);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_names() {
        assert_eq!(DeviceKind::Thermometer.json_name(), "thermometer");
        assert_eq!(DeviceKind::BloodPressure.json_name(), "blood_pressure");
    }

    #[test]
    fn jp_names() {
        assert_eq!(DeviceKind::Thermometer.jp_name(), "体温計");
        assert_eq!(DeviceKind::BloodPressure.jp_name(), "血圧計");
    }

    #[test]
    fn matches_thermometer_names() {
        assert_eq!(match_device_name("NT-100B"), Some(DeviceKind::Thermometer));
        assert_eq!(
            match_device_name("MyThermometer"),
            Some(DeviceKind::Thermometer)
        );
    }

    #[test]
    fn matches_blood_pressure_names() {
        assert_eq!(
            match_device_name("NBP-1BLE"),
            Some(DeviceKind::BloodPressure)
        );
        assert_eq!(match_device_name("BP-Meter"), Some(DeviceKind::BloodPressure));
        assert_eq!(
            match_device_name("BloodPressure"),
            Some(DeviceKind::BloodPressure)
        );
    }

    #[test]
    fn unknown_name() {
        assert_eq!(match_device_name("FC-1200"), None);
    }
}
