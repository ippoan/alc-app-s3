//! I/O 層 (host_link / ble) から UI へ送る画面操作コマンド。
//!
//! UI 本体 (状態機械・描画) はバイナリ側 (src/ui) にあり、本クレートは
//! この enum を送るだけ。UI 側の Screen 遷移規則は src/ui/mod.rs を参照。

/// ホスト / BLE からの画面操作コマンド
pub enum UiCommand {
    ShowQr {
        payload: String,
        timeout_ms: u64,
    },
    Measure,
    Result {
        ok: bool,
        value: String,
    },
    Error {
        message: String,
    },
    Reset,
    /// 画面向き変更 (0/90/180/270 度)。NVS への保存は host_link 側で実施済み
    Rotate(u16),
    /// BLE 体温計の測定値 (℃)
    Temperature {
        celsius: f32,
    },
    /// BLE 血圧計の測定値 (mmHg)
    BloodPressure {
        systolic: f32,
        diastolic: f32,
        pulse: Option<f32>,
    },
}
