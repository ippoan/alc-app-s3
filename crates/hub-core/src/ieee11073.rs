//! IEEE 11073 数値フォーマットのデコード。
//!
//! ble-medical-gateway の Arduino 版 (`src/main.cpp` の parseTemperature /
//! parseBloodPressure) を移植。Temperature Measurement (0x2A1C) /
//! Blood Pressure Measurement (0x2A35) のペイロードを解釈する。

/// Temperature Measurement (0x2A1C): IEEE 11073 FLOAT (32bit)。
/// 摂氏に正規化して返す。ペイロード不足時は None。
pub fn parse_temperature(data: &[u8]) -> Option<f32> {
    if data.len() < 5 {
        return None;
    }
    let flags = data[0];
    let fahrenheit = flags & 0x01 != 0;

    let mut mantissa = i32::from(data[1]) | (i32::from(data[2]) << 8) | (i32::from(data[3]) << 16);
    if mantissa & 0x0080_0000 != 0 {
        mantissa |= 0xFF00_0000u32 as i32; // 符号拡張
    }
    let exponent = data[4] as i8;

    let mut t = mantissa as f32 * 10f32.powi(i32::from(exponent));
    if fahrenheit {
        t = (t - 32.0) * 5.0 / 9.0;
    }
    Some(t)
}

/// Blood Pressure Measurement (0x2A35) のデコード結果 (mmHg)
#[derive(Debug, Clone, PartialEq)]
pub struct BloodPressure {
    pub systolic: f32,
    pub diastolic: f32,
    pub pulse: Option<f32>,
    /// 測定時刻 (機器内蔵時計)。YYYYMMDDHHMMSS を u64 に詰めた比較用の値。
    /// 血圧計は保存済みの過去測定をまとめて送るため、これで最新の 1 件を選ぶ。
    /// タイムスタンプ非搭載の機器は None。
    pub timestamp: Option<u64>,
}

/// IEEE 11073 SFLOAT (16bit)
pub fn sfloat(lo: u8, hi: u8) -> f32 {
    let mut mantissa = i16::from(lo) | (i16::from(hi & 0x0F) << 8);
    if mantissa & 0x0800 != 0 {
        mantissa |= 0xF000u16 as i16; // 符号拡張
    }
    let mut exponent = (hi >> 4) as i8;
    if exponent & 0x08 != 0 {
        exponent |= 0xF0u8 as i8; // 符号拡張
    }
    f32::from(mantissa) * 10f32.powi(i32::from(exponent))
}

/// Blood Pressure Measurement (0x2A35)。kPa 表記は mmHg に換算して返す。
pub fn parse_blood_pressure(data: &[u8]) -> Option<BloodPressure> {
    if data.len() < 7 {
        return None;
    }
    let flags = data[0];
    let is_kpa = flags & 0x01 != 0;
    let has_timestamp = flags & 0x02 != 0;
    let has_pulse = flags & 0x04 != 0;

    let mut systolic = sfloat(data[1], data[2]);
    let mut diastolic = sfloat(data[3], data[4]);
    // data[5..7] は Mean Arterial Pressure (未使用)

    // タイムスタンプ (7 バイト: 年 LE 2, 月, 日, 時, 分, 秒) を data[7..14] から読む
    let timestamp = if has_timestamp && data.len() >= 14 {
        let year = u64::from(data[7]) | (u64::from(data[8]) << 8);
        let month = u64::from(data[9]);
        let day = u64::from(data[10]);
        let hour = u64::from(data[11]);
        let min = u64::from(data[12]);
        let sec = u64::from(data[13]);
        Some(
            year * 10_000_000_000
                + month * 100_000_000
                + day * 1_000_000
                + hour * 10_000
                + min * 100
                + sec,
        )
    } else {
        None
    };

    let mut offset = 7;
    if has_timestamp {
        offset += 7; // タイムスタンプは 7 バイト
    }
    let pulse = if has_pulse && offset + 2 <= data.len() {
        Some(sfloat(data[offset], data[offset + 1]))
    } else {
        None
    };

    if is_kpa {
        systolic *= 7.50062;
        diastolic *= 7.50062;
    }
    Some(BloodPressure {
        systolic,
        diastolic,
        pulse,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn sfloat_positive() {
        assert_eq!(sfloat(120, 0), 120.0);
    }

    #[test]
    fn sfloat_negative_mantissa() {
        // mantissa 0xFFF = -1, exponent 0
        assert_eq!(sfloat(0xFF, 0x0F), -1.0);
    }

    #[test]
    fn sfloat_negative_exponent() {
        // mantissa 0x16D = 365, exponent 0xF = -1 → 36.5
        assert!(approx(sfloat(0x6D, 0xF1), 36.5));
    }

    #[test]
    fn temperature_too_short() {
        assert_eq!(parse_temperature(&[0x00, 0x6D, 0x01, 0x00]), None);
    }

    #[test]
    fn temperature_celsius() {
        // mantissa 365, exponent -1 → 36.5°C
        let t = parse_temperature(&[0x00, 0x6D, 0x01, 0x00, 0xFF]).unwrap();
        assert!(approx(t, 36.5));
    }

    #[test]
    fn temperature_fahrenheit_converted() {
        // mantissa 986, exponent -1 → 98.6°F → 37.0°C
        let t = parse_temperature(&[0x01, 0xDA, 0x03, 0x00, 0xFF]).unwrap();
        assert!(approx(t, 37.0));
    }

    #[test]
    fn temperature_negative_mantissa_sign_extended() {
        // mantissa 0x800000 → 符号拡張で負値
        let t = parse_temperature(&[0x00, 0x00, 0x00, 0x80, 0x00]).unwrap();
        assert!(t < 0.0);
    }

    #[test]
    fn blood_pressure_too_short() {
        assert_eq!(parse_blood_pressure(&[0x00; 6]), None);
    }

    #[test]
    fn blood_pressure_basic_mmhg() {
        let bp = parse_blood_pressure(&[0x00, 120, 0, 80, 0, 0, 0]).unwrap();
        assert_eq!(bp.systolic, 120.0);
        assert_eq!(bp.diastolic, 80.0);
        assert_eq!(bp.pulse, None);
        assert_eq!(bp.timestamp, None); // タイムスタンプ非搭載
    }

    #[test]
    fn blood_pressure_timestamp_flag_but_truncated() {
        // timestamp フラグは立つがデータが 14 バイト未満 → timestamp None
        let bp = parse_blood_pressure(&[0x02, 120, 0, 80, 0, 0, 0]).unwrap();
        assert_eq!(bp.timestamp, None);
    }

    #[test]
    fn blood_pressure_with_pulse() {
        let bp = parse_blood_pressure(&[0x04, 120, 0, 80, 0, 0, 0, 72, 0]).unwrap();
        assert_eq!(bp.pulse, Some(72.0));
    }

    #[test]
    fn blood_pressure_with_timestamp_and_pulse() {
        // flags: timestamp(0x02) + pulse(0x04)。pulse はタイムスタンプ 7 バイトの後
        let data = [
            0x06, 120, 0, 80, 0, 0, 0, // 基本 7 バイト
            0xE9, 0x07, 1, 2, 3, 4, 5, // タイムスタンプ 7 バイト
            72, 0, // pulse
        ];
        let bp = parse_blood_pressure(&data).unwrap();
        assert_eq!(bp.pulse, Some(72.0));
        // 年 0x07E9=2025, 月1, 日2, 時3, 分4, 秒5 → 20250102030405
        assert_eq!(bp.timestamp, Some(20_250_102_030_405));
    }

    #[test]
    fn blood_pressure_pulse_flag_but_truncated() {
        // pulse フラグは立っているがデータが足りない → None
        let bp = parse_blood_pressure(&[0x04, 120, 0, 80, 0, 0, 0]).unwrap();
        assert_eq!(bp.pulse, None);
    }

    #[test]
    fn blood_pressure_kpa_converted() {
        // 16 kPa → 120.01 mmHg / 8 kPa → 60.00 mmHg
        let bp = parse_blood_pressure(&[0x01, 16, 0, 8, 0, 0, 0]).unwrap();
        assert!(approx(bp.systolic, 120.01));
        assert!(approx(bp.diastolic, 60.0));
    }
}
