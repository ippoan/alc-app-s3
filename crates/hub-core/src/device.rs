//! 対象 BLE 機器 (ニプロ体温計/血圧計、Omron 血圧計) の種別と判定。

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

/// Omron の BLE 機器 (HEM-6231T) の広告がどちらの状態か。
/// どちらも service UUID を広告しないので名前で見分ける (Refs #237、Linux で実測)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmronAdv {
    /// ペアリング待ち (機器の -P- 点滅中)。unlock 鍵の登録に接続する
    Pairing,
    /// 測定後の送信。bond 済みで接続し 0x2A35 を受け取る
    Transfer,
}

/// 広告名から Omron 機器の状態を返す。接頭辞の大文字小文字で状態が変わる
/// (ペアリング待ち `BLEsmart_`、送信 `BLESmart_`) ので区別して比べる。
pub fn omron_adv(name: &str) -> Option<OmronAdv> {
    if name.starts_with("BLEsmart_") {
        Some(OmronAdv::Pairing)
    } else if name.starts_with("BLESmart_") {
        Some(OmronAdv::Transfer)
    } else {
        None
    }
}

/// アドバタイズのデバイス名から種別を判定する (Arduino 版の名前判定を移植。
/// ニプロ機器が標準サービス UUID を広告しない場合の対策)。
/// Omron 機器 (独自名) も血圧計として拾う。
pub fn match_device_name(name: &str) -> Option<DeviceKind> {
    if omron_adv(name).is_some() {
        return Some(DeviceKind::BloodPressure);
    }
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

    #[test]
    fn omron_adv_states() {
        assert_eq!(omron_adv("BLEsmart_test"), Some(OmronAdv::Pairing));
        assert_eq!(omron_adv("BLESmart_test"), Some(OmronAdv::Transfer));
        assert_eq!(omron_adv("blesmart_test"), None);
        assert_eq!(omron_adv("NBP-1BLE"), None);
    }

    #[test]
    fn matches_omron_names() {
        assert_eq!(
            match_device_name("BLEsmart_test"),
            Some(DeviceKind::BloodPressure)
        );
        assert_eq!(
            match_device_name("BLESmart_test"),
            Some(DeviceKind::BloodPressure)
        );
    }
}
