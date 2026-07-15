//! 定数定義。
//!
//! ピン割当は main.rs で型レベル (GpioNN) に固定している。根拠は
//! `ippoan/alc-app` の `plan/cores3-hub-consolidation.md` (issue #102 参照):
//!
//! - RS232M Module 13.2 → DB9 → FC-1200: TX=G17 / RX=G18 (DIP スイッチ候補)。
//!   G13/G0/G14 は CoreS3 内蔵 I2S が使用済みのため使用不可。
//!   LAN Module のデフォルト INT (G10) との競合を避けるため G17/G18 を選択。
//!   シルク印刷の番号とコードの GPIO 番号が一致しない実例あり
//!   (M5Stack Community #5581) — 実機で要確認。
//! - LAN Module 13.2 (W5500): CS=G1 / RST=G0 / INT=G10 (LinkStatus.ino 既定)。
//!   ジャンパで INT=G34 / RST=G13 / CS=G15 へ変更可。

pub const FIRMWARE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// ビルド時の git 短縮 SHA (build.rs が埋め込む。git 無しは "dev")。
pub const FIRMWARE_SHA: &str = env!("FW_GIT_SHA");

/// 完全なバージョン識別子 `<version>+<sha>`。CI が manifest.json に載せる形式
/// (`<version>+<short-sha>`) と一致するので、OTA の更新要否判定に使える。
pub fn firmware_version_full() -> String {
    format!("{FIRMWARE_VERSION}+{FIRMWARE_SHA}")
}

/// FC-1200 側 RS232 ボーレート (タニタ FC-1200/ALBLO 仕様: 9600bps 8N1、fc1200-wasm README 準拠)
pub const RS232_BAUD: u32 = 9_600;

/// QR 画面の既定有効期限
pub const QR_DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// 測定結果画面の自動クローズまでの時間
pub const RESULT_AUTO_CLOSE_MS: u64 = 10_000;

/// RS232 を「受信あり」とみなす直近受信ウィンドウ
pub const RS232_ACTIVE_WINDOW_MS: u64 = 5_000;

/// 体温/血圧表示画面の自動クローズまでの時間
pub const VITALS_AUTO_CLOSE_MS: u64 = 30_000;

/// 点呼画面の測定待ちタイムアウト。体温→血圧と続けて測る時間を確保する
/// ため長めに取る (超過で待機画面へ戻る)
pub const TENKO_TIMEOUT_MS: u64 = 180_000;

/// 点呼画面で体温・血圧の両方が揃ってから待機画面へ戻るまでの時間
pub const TENKO_DONE_CLOSE_MS: u64 = 5_000;

/// 無操作 (タッチ・ホスト/BLE コマンドなし) が続いた時にバックライトを
/// 最低輝度まで落とすまでの時間。常時表示の画面焼け対策の一環
/// (Refs C_TEXT 減光)。完全消灯にしないのは、本体が動作中か外から
/// 見分けが付かなくなるため
pub const BACKLIGHT_IDLE_DIM_MS: u64 = 60_000;

/// auth-worker の既定ベース URL (`AUTH URL` コマンドで NVS 上書き可)。
/// provisioning は USB 前提: credential はホストが `/device/pair` 系で取得し
/// `AUTH SET` で注入する (role は host 側で `device-hub` を指定)
pub const AUTH_WORKER_URL_DEFAULT: &str = "https://auth.ippoan.org";

/// cf-alc-recorder の既定 WS URL (`WS URL` コマンドで NVS 上書き可。
/// staging は wss://alc-recorder-staging.m-tama-ramu.workers.dev/ws)
pub const RECORDER_WS_URL_DEFAULT: &str = "wss://alc-recorder.m-tama-ramu.workers.dev/ws";
