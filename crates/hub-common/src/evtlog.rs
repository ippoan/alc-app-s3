//! `EVT ` 行の出口 (Refs ippoan/alc-app-s3#215)。
//!
//! `EVT ` 行はホスト (キオスク PWA / setup ページ) 向けに `println!` で出すが、
//! `println!` は crashlog の vprintf hook を通らないため `.noinit` リング
//! (= WS 下り `get_log` / `LOG DUMP` / crash_log で遠隔から読めるログ) に残らない。
//! [`emit`] は `println!` に加えて、[`set_sink`] で登録された書き込み口にも同じ行を
//! 渡す。登録するのは `alc_hub_drivers::crashlog::init` (= `crashlog::note`) で、
//! CoreS3 と AtomS3 の全機種に効く。hub-ui / hub-ble は hub-drivers に依存しない
//! ため、口をここ (全員が依存する hub-common) に置いている。
//!
//! # どの行を emit にするか
//!
//! **`EVT ` で始まる行は原則 [`emit`]。** 例外は `println!` のまま残す:
//!
//! - **周期的なもの** (60 秒以内の周期で出る `EVT BATT` / `EVT HEAP` /
//!   `EVT OTA_PROGRESS` 等) — 4 KB のリングを数十秒で押し流し、切り分けに要る
//!   出来事を消してしまう
//! - **人・カード・セッション・資格情報・設置場所を特定できる値を含むもの**
//!   (`EVT TENKO_SESSION` / `EVT NFC_LICENSE` / `EVT TIMECARD` / `EVT AUTH_TOKEN` /
//!   URL や SSID を含む行 等) — リングは get_device_log で遠隔から読まれ、そこから
//!   issue / PR の本文へ転記されうる。その経路を作らない
//! - **CoreS3 の `EVT ALARM`** — シリアルにも出さない (hub-drivers/src/alarm.rs)
//!
//! 画面を変えるホストのコマンドへの `OK` 応答 (host_link.rs の QR / MEASURE /
//! RESULT / ERROR / RESET / STAGE) も emit にする (PC 主導の点呼の進みを
//! get_log で後から読むため、Refs #135)

use std::sync::OnceLock;

static SINK: OnceLock<fn(&str)> = OnceLock::new();

/// [`emit`] の行の書き込み口を登録する。最初の登録だけが効く (起動時に 1 回)。
pub fn set_sink(sink: fn(&str)) {
    let _ = SINK.set(sink);
}

/// `EVT ` 行をホストへ出し (`println!`)、登録済みならリングにも残す。
pub fn emit(line: &str) {
    println!("{line}");
    if let Some(sink) = SINK.get() {
        sink(line);
    }
}
