//! 測定値のクレート間受け渡し型。
//!
//! BLE の notify コールバック (nimble_host タスク上・スタック小) は
//! 「パースして本型を channel へ送る」だけに留め、重い処理 (JSON 出力・
//! NVS 記録・画面通知) は recorder スレッドで行う。これによりコールバックの
//! スタック消費を最小化し、速攻計測 (体温→血圧の連続) でもクラッシュしない。

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
}
