//! 印刷ブリッジのホストコンソール (USB Serial/JTAG、行指向)。
//!
//! 読み出しスレッドと機種非依存のコマンド (PING / HEAP / LOG / AUTH / WS) は
//! `alc_hub_drivers::console` が持つ。ここに書くのは**印刷ブリッジ固有の分岐だけ**。
//! 行解析は alc_hub_core::protocol::parse_line を共有し、本機で意味を持たない
//! コマンド (QR/MEASURE/BLE 等) は `ERR UNSUPPORTED` を返す。
//! Improv Wi-Fi Serial は受けない (Wi-Fi 無し・LAN 専用)。
//!
//! # 対応コマンド (ホスト → AtomS3)
//!
//! | コマンド | 説明 |
//! |---|---|
//! | `PING` | 疎通確認 (`PONG` 応答、共通実装) |
//! | `STATUS` | `STATUS LAN=1 IP=192.168.11.52 PRINTER=host:9100` 応答 |
//! | `HEAP` / `HEAP DUMP` / `LOG DUMP` | ヒープ概況 / 詳細 / 直近ログ (共通実装) |
//! | `OTA <url>` | オンラインアップデート (`EVT OTA_*`、ota.rs) |
//! | `PRINT <url>` | PDF を取得しプリンターへ 9100 送信 (`EVT PRINT_*`) |
//! | `PRINTER ADDR <host:port>` | プリンター宛先の保存 (NVS) |
//! | `PRINTER STATUS` | `PRINTER <addr>` / `PRINTER UNSET` 応答 |
//! | `AUTH SET/UNPAIR/STATUS/TOKEN/URL` | device credential 管理 (共通実装 `console::handle_common`。/device/setup ページからの provisioning 用) |
//! | `WS URL <url>` / `WS STATUS` | cf-alc-recorder 常時接続の URL 上書き / 状態 (共通実装) |

use alc_hub_core::protocol::{parse_line, HostCommand};
use anyhow::Result;

use alc_hub_common::{config, settings::Settings, status::SharedStatus};
use alc_hub_drivers::{console, printer};

pub fn start(status: SharedStatus, settings: Settings) -> Result<()> {
    console::spawn_reader(c"console", 8 * 1024, move |line| {
        handle_line(line, &status, &settings)
    })
}

fn handle_line(line: &str, status: &SharedStatus, settings: &Settings) {
    let command = match parse_line(line, 0) {
        Ok(Some(command)) => command,
        Ok(None) => return, // 空行
        Err(err_response) => {
            println!("{err_response}");
            return;
        }
    };

    // 機種に依らないコマンドは共通実装へ (console.rs)。
    // 捌かれなかったものだけがここへ落ちてくる
    let Some(command) = console::handle_common(command, status, settings) else {
        return;
    };

    match command {
        HostCommand::Status => {
            let (lan, ip) = status
                .lock()
                .map(|s| (s.lan_link, s.lan_ip.clone()))
                .unwrap_or_default();
            println!(
                "STATUS LAN={} IP={} PRINTER={} VER={}",
                u8::from(lan),
                if ip.is_empty() { "-" } else { &ip },
                settings.printer_addr().as_deref().unwrap_or("-"),
                config::firmware_version_full(),
            );
        }
        // オンラインアップデート (進捗・結果は EVT OTA_*)
        HostCommand::Ota { url } => console::handle_ota_lan_guarded(url, status),
        // 印刷 (進捗・結果は EVT PRINT_*)
        HostCommand::Print { url } => match settings.printer_addr() {
            Some(addr) => {
                printer::spawn_print(url, addr, status.clone());
                println!("OK PRINT");
            }
            None => println!("ERR PRINT: 宛先未設定 (PRINTER ADDR host:port で設定してください)"),
        },
        HostCommand::PrinterAddr { addr } => match settings.set_printer_addr(&addr) {
            Ok(()) => println!("OK PRINTER ADDR"),
            Err(e) => {
                log::error!("console: printer_addr 保存失敗: {e:?}");
                println!("ERR PRINTER: 保存に失敗しました");
            }
        },
        HostCommand::PrinterStatus => match settings.printer_addr() {
            Some(addr) => println!("PRINTER {addr}"),
            None => println!("PRINTER UNSET"),
        },
        // 本機で意味を持たないコマンド (画面遷移 / BLE / Wi-Fi / CFG 等)
        other => {
            log::debug!("console: unsupported command: {other:?}");
            println!("ERR UNSUPPORTED (print hub)");
        }
    }
}
