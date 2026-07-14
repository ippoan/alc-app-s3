//! 印刷ブリッジのホストコンソール (USB Serial/JTAG、行指向)。
//!
//! CoreS3 の host_link.rs から「印刷ブリッジに必要なコマンドだけ」を
//! 実装した縮小版。行解析は alc_hub_core::protocol::parse_line を共有し、
//! 本機で意味を持たないコマンド (QR/MEASURE/BLE 等) は `ERR UNSUPPORTED`
//! を返す。Improv Wi-Fi Serial は受けない (Wi-Fi 無し・LAN 専用)。
//!
//! # 対応コマンド (ホスト → AtomS3)
//!
//! | コマンド | 説明 |
//! |---|---|
//! | `PING` | 疎通確認 (`PONG` 応答) |
//! | `STATUS` | `STATUS LAN=1 IP=192.168.11.52 PRINTER=host:9100` 応答 |
//! | `HEAP` / `HEAP DUMP` | ヒープ概況 / 詳細 (CoreS3 と同形式) |
//! | `OTA <url>` | オンラインアップデート (`EVT OTA_*`、ota.rs) |
//! | `PRINT <url>` | PDF を取得しプリンターへ 9100 送信 (`EVT PRINT_*`) |
//! | `PRINTER ADDR <host:port>` | プリンター宛先の保存 (NVS) |
//! | `PRINTER STATUS` | `PRINTER <addr>` / `PRINTER UNSET` 応答 |
//! | `AUTH SET/UNPAIR/STATUS/TOKEN/URL` | device credential 管理 (CoreS3 と同形式。/device/setup ページからの provisioning 用) |
//! | `WS URL <url>` / `WS STATUS` | cf-alc-recorder 常時接続の URL 上書き / 状態 |

use std::io::Read;

use alc_hub_core::protocol::{parse_line, HostCommand};
use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys;

use alc_hub_common::{config, settings::Settings, status::SharedStatus};
use alc_hub_drivers::{auth_link, heap, ota, printer};

/// 行としてバッファする最大長 (超えたら読み捨て — バイナリノイズ対策)
const MAX_LINE: usize = 512;

pub fn start(status: SharedStatus, settings: Settings) -> Result<()> {
    // USB Serial/JTAG ドライバを VFS に接続し stdin のブロッキング読み出しを
    // 可能にする (CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y 前提、host_link.rs と同じ)
    unsafe {
        let mut cfg = sys::usb_serial_jtag_driver_config_t {
            tx_buffer_size: 1024,
            rx_buffer_size: 1024,
        };
        sys::usb_serial_jtag_driver_install(&mut cfg);
        sys::esp_vfs_usb_serial_jtag_use_driver();
    }

    std::thread::Builder::new()
        .name("console".into())
        .stack_size(8 * 1024)
        .spawn(move || {
            let mut chunk = [0u8; 64];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                match std::io::stdin().lock().read(&mut chunk) {
                    Ok(0) => FreeRtos::delay_ms(20),
                    Ok(n) => {
                        acc.extend_from_slice(&chunk[..n]);
                        drain_lines(&mut acc, &status, &settings);
                    }
                    Err(_) => FreeRtos::delay_ms(100),
                }
            }
        })?;
    Ok(())
}

/// バッファから完成した行を取り出して処理する
fn drain_lines(acc: &mut Vec<u8>, status: &SharedStatus, settings: &Settings) {
    loop {
        let Some(pos) = acc.iter().position(|&b| b == b'\n' || b == b'\r') else {
            if acc.len() > MAX_LINE {
                acc.clear();
            }
            return;
        };
        let line_bytes: Vec<u8> = acc.drain(..=pos).collect();
        let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]);
        handle_line(line.trim(), status, settings);
    }
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

    match command {
        HostCommand::Ping => println!("PONG"),
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
        HostCommand::Heap => {
            let s = heap::stats();
            println!(
                "HEAP FREE_INT={} MIN_INT={} FREE_PSRAM={} TOTAL_INT={} TOTAL_PSRAM={}",
                s.free_int, s.min_int, s.free_psram, s.total_int, s.total_psram,
            );
        }
        HostCommand::HeapDump => heap::dump(),
        // オンラインアップデート (進捗・結果は EVT OTA_*)。
        // LAN 確立前に lwip を叩くと assert リブートするためガードする
        // (printer::spawn_print と同じ理由)
        HostCommand::Ota { url } => {
            let lan_up = status.lock().map(|s| s.lan_link).unwrap_or(false);
            if lan_up {
                ota::spawn_update(url, status.clone(), None);
                println!("OK OTA");
            } else {
                println!("ERR OTA: LAN 未接続 (ETH_CONNECTED を待ってください)");
            }
        }
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
        // device credential 管理 (host_link.rs と同じ応答形式 —
        // /device/setup ページの provisioning フローがそのまま使えるように)
        HostCommand::AuthSet {
            device_id,
            device_secret,
            tenant_id,
        } => match settings.set_device_credential(&device_id, &device_secret, &tenant_id) {
            Ok(()) => println!("OK AUTH SET"),
            Err(e) => {
                log::error!("console: credential 保存失敗: {e:?}");
                println!("ERR AUTH: credential の保存に失敗しました");
            }
        },
        HostCommand::AuthUnpair => match settings.clear_device_credential() {
            Ok(()) => println!("OK AUTH UNPAIR"),
            Err(e) => {
                log::error!("console: credential 破棄失敗: {e:?}");
                println!("ERR AUTH: 破棄に失敗しました");
            }
        },
        HostCommand::AuthStatus => match settings.device_credential() {
            Some((id, _)) => println!(
                "AUTH PAIRED {} {}",
                settings.device_tenant().unwrap_or_default(),
                id,
            ),
            None => println!("AUTH UNPAIRED"),
        },
        HostCommand::AuthToken => {
            auth_link::spawn_mint_test(settings.clone(), status.clone());
            println!("OK AUTH TOKEN");
        }
        HostCommand::AuthUrl { url } => match settings.set_auth_url(&url) {
            Ok(()) => println!("OK AUTH URL"),
            Err(e) => {
                log::error!("console: auth URL 保存失敗: {e:?}");
                println!("ERR AUTH: URL の保存に失敗しました");
            }
        },
        // cf-alc-recorder 常時接続 (ws_uplink.rs — 下り print/ota command 待受)
        HostCommand::WsUrl { url } => match settings.set_ws_url(&url) {
            Ok(()) => println!("OK WS URL"),
            Err(e) => {
                log::error!("console: WS URL 保存失敗: {e:?}");
                println!("ERR WS: URL の保存に失敗しました");
            }
        },
        HostCommand::WsStatus => {
            let st = status.lock().map(|s| s.clone()).unwrap_or_default();
            println!(
                "WS CONNECTED={} QUEUE={} SEQ={}",
                u8::from(st.ws_connected),
                st.ws_queue_len,
                st.ws_last_seq,
            );
        }
        // 本機で意味を持たないコマンド (画面遷移 / BLE / Wi-Fi / CFG 等)
        other => {
            log::debug!("console: unsupported command: {other:?}");
            println!("ERR UNSUPPORTED (print hub)");
        }
    }
}
