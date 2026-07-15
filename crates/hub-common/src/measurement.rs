//! 測定値のクレート間受け渡し型。
//!
//! BLE の notify コールバック (nimble_host タスク上・スタック小) は
//! 「パースして本型を channel へ送る」だけに留め、重い処理 (JSON 出力・
//! NVS 記録・画面通知) は recorder スレッドで行う。これによりコールバックの
//! スタック消費を最小化し、速攻計測 (体温→血圧の連続) でもクラッシュしない。

/// WS 送信 (cf-alc-recorder) へ fan-out する 1 レコード。
/// recorder スレッドがホスト向け JSON と同じ payload を積む
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UplinkRecord {
    /// cf-alc-recorder の kind (temperature / blood_pressure / alcohol /
    /// fc1200_raw / crash_log)
    pub kind: &'static str,
    /// ble-medical-gateway 互換 JSON オブジェクト文字列
    pub payload: String,
    /// 記録時刻 (epoch ms、NTP 未同期時は稼働時間由来の値になり得る)
    pub recorded_at_ms: u64,
}

/// 1 回の測定結果 (alloc 不要の Copy 型)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Measurement {
    Temperature {
        celsius: f32,
        /// 機器内蔵時計の測定時刻 (YYYYMMDDHHMMSS)。同一測定の再送を
        /// 見分ける重複排除に使う。タイムスタンプ非搭載の機器は None
        timestamp: Option<u64>,
        /// 受信時刻 (稼働 ms)
        at_ms: u64,
    },
    BloodPressure {
        systolic: f32,
        diastolic: f32,
        pulse: Option<f32>,
        /// 機器内蔵時計の測定時刻 (YYYYMMDDHHMMSS)。重複排除に使う
        timestamp: Option<u64>,
        at_ms: u64,
    },
    /// FC-1200 (RS232) のアルコール測定。値は 0.01mg/L 単位の整数
    Alcohol {
        result: alc_hub_core::fc1200::AlcoholResult,
        centi_mg_per_l: u16,
        /// 機器の累計使用回数。同一測定の再送 (RSOK 取りこぼし時) の重複排除に使う
        use_count: u32,
        at_ms: u64,
    },
}
