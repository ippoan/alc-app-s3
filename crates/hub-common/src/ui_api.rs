//! I/O 層 (host_link / ble) から UI へ送る画面操作コマンド。
//!
//! UI 本体 (状態機械・描画) はバイナリ側 (src/ui) にあり、本クレートは
//! この enum を送るだけ。UI 側の Screen 遷移規則は src/ui/mod.rs を参照。

use alc_hub_core::device::DeviceKind;

/// FC-1200 (RS232) の測定進行状態。点呼画面のアルコール欄に
/// 「計測待ち」の代わりのライブ表示を出すために rs232.rs が送る
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlcoholStage {
    /// ウォームアップ中 (UT 受信〜MSWM 待ち) → 「準備中」
    Warming,
    /// 吹込待ち (MSWM 受信後) → 「吹込待ち」
    BlowWaiting,
    /// 吹込完了・判定中 (MSEN 受信後) → 「判定中」
    Measuring,
}

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
    /// BLE 機器への接続開始 (点呼画面のラベル横に取得中スピナーを表示)
    BleAcquiring { device: DeviceKind },
    /// BLE 接続の終了 — 切断/再スキャン (スピナー消去)
    BleIdle,
    /// FC-1200 の測定進行状態 (None = 待機へ戻った: タイムアウト等)。
    /// 点呼画面表示中のみアルコール欄に反映される
    AlcoholStage(Option<AlcoholStage>),
}
