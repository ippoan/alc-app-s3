//! 警告デバイスのホストコンソール (USB Serial/JTAG、行指向)。
//!
//! 読み出しスレッドと機種非依存のコマンド (PING / HEAP / LOG / AUTH / WS) は
//! `alc_hub_drivers::console` が持つ。ここに書くのは**本機固有の 2 分岐だけ**
//! (`HB` と `STATUS`)。**他機から丸写ししないこと** — とくに `AUTH SET`
//! (device credential を NVS へ書く口) を機種ごとに増やすと provisioning が割れる。
//!
//! # 対応コマンド (キオスク → 端末)
//!
//! | コマンド | 説明 |
//! |---|---|
//! | `HB OK` / `HB NG <reason>` | heartbeat。3 秒ごと。末尾に任意で `call=0` / `call=1`。**返信しない** |
//! | `STATUS` | `STATUS alarm state=… cause=… hb_age_ms=… VER=…` 応答 |
//! | `PING` | 疎通確認 (`PONG` 応答、共通実装) |
//! | `HEAP` / `HEAP DUMP` / `LOG DUMP` | ヒープ概況 / 詳細 / 直近ログ (共通実装) |
//!
//! 本機で意味を持たないもの (QR / MEASURE / BLE / 印刷 / OTA / Wi-Fi) は
//! `ERR UNSUPPORTED (alarm)` を返す。**OTA も無い** — 更新は Pages インストーラ
//! (docs/alarm.html) から USB で焼き直す。
//!
//! # 端末 → キオスク
//!
//! `EVT ALARM state=<idle|alarming|muted> cause=<none|silence|ng:<reason>|call>` を
//! 状態遷移のたび + `alarm::BANNER_MS` ごとに出す (出すのは main の鳴動ループ)。
//!
//! ★ **`STATUS` 応答の先頭 2 トークン `STATUS alarm` は変えないこと。**
//!   ブラウザ側 (alc-app) は**この 2 トークンで機種を識別する** — CoreS3 と
//!   VoiceS3R は USB の VID/PID が同一 (0x303A:0x1001) で、記述子では
//!   見分けられない。

use alc_hub_common::{config, settings::Settings, status::SharedStatus};
use alc_hub_core::alarm::AlarmMonitor;
use alc_hub_core::protocol::{parse_line, HostCommand};
use alc_hub_drivers::console;
use anyhow::Result;
use std::sync::{Arc, Mutex};

/// 鳴動判定の共有ハンドル (main の鳴動ループと共有)
pub type SharedMonitor = Arc<Mutex<AlarmMonitor>>;

pub fn start(monitor: SharedMonitor, status: SharedStatus, settings: Settings) -> Result<()> {
    console::spawn_reader(c"console", 8 * 1024, move |line| {
        handle_line(line, &monitor, &status, &settings)
    })
}

fn handle_line(line: &str, monitor: &SharedMonitor, status: &SharedStatus, settings: &Settings) {
    let command = match parse_line(line, 0) {
        Ok(Some(command)) => command,
        Ok(None) => return, // 空行
        Err(err_response) => {
            println!("{err_response}");
            return;
        }
    };

    // 機種に依らないコマンドは共通実装へ (hub-drivers/src/console.rs)。
    // 捌かれなかったものだけがここへ落ちてくる
    let Some(command) = console::handle_common(command, status, settings) else {
        return;
    };

    match command {
        // heartbeat。**応答を返さない** — 3 秒ごとに来るので返すとログが埋まる。
        // 状態遷移はここでは起こさず、次の tick (main の鳴動ループ) が判定する。
        // reason が無い `HB NG` も受理する (monitor が
        // `alarm::DEFAULT_NG_REASON` に落として `cause=ng:unspecified` にする)
        HostCommand::Heartbeat { ok, reason, call } => {
            with_monitor(monitor, |m, now| {
                m.on_heartbeat(now, ok, reason.as_deref(), call)
            });
        }
        // `status_line` は `VER=` を含まない (hub-core からは hub-common が
        // 見えないため)。**呼び出し側で末尾に足す**
        HostCommand::Status => {
            with_monitor(monitor, |m, now| {
                println!(
                    "{} VER={}",
                    m.status_line(now),
                    config::firmware_version_full()
                );
            });
        }
        // 本機で意味を持たないコマンド (画面遷移 / 測定 / BLE / 印刷 / OTA / Wi-Fi)
        other => {
            log::debug!("console: unsupported command: {other:?}");
            println!("ERR UNSUPPORTED (alarm)");
        }
    }
}

/// 判定器を lock して現在時刻とともに渡す。
///
/// **lock 失敗時に黙って捨てない** — heartbeat を落とすと沈黙とみなされて
/// 鳴り出すので、なぜ落としたかがログに残っている必要がある
fn with_monitor(monitor: &SharedMonitor, f: impl FnOnce(&mut AlarmMonitor, u64)) {
    match monitor.lock() {
        Ok(mut m) => {
            let now = alc_hub_common::status::now_ms();
            f(&mut m, now)
        }
        Err(e) => log::error!("console: monitor の lock に失敗: {e}"),
    }
}
