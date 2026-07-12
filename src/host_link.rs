//! ホストリンク: CoreS3 ネイティブ USB-C (USB Serial/JTAG) 経由の
//! 行指向テキストプロトコル。
//!
//! 接続相手は plan/cores3-hub-consolidation.md の構成に従い、
//! 近い将来計画では Windows PC (alc-app キオスク)、Windows 排除案では
//! 固定 Android タブレット (Host/OTG モード)。
//!
//! # 受信コマンド (ホスト → CoreS3)
//!
//! | コマンド | 説明 |
//! |---|---|
//! | `PING` | 疎通確認。`PONG` を返す |
//! | `QR <payload> [timeout_s]` | QR コード画面を表示 (顔認証後のトークン等) |
//! | `MEASURE` | 測定中画面を表示 |
//! | `RESULT OK\|NG [value]` | 測定結果画面を表示 (value 例: `0.000`) |
//! | `ERROR <message>` | エラー画面を表示 |
//! | `RESET` | 待機画面へ戻す |
//! | `STATUS` | `STATUS LAN=0 RS232=1 BLE=0` を返す |
//!
//! # 送信イベント (CoreS3 → ホスト)
//!
//! | イベント | 説明 |
//! |---|---|
//! | `FC1200 <hex>` | RS232 (FC-1200) からの受信データ (パススルー) |
//! | `EVT QR_TIMEOUT` | QR 画面が有効期限切れで閉じた |
//! | `EVT RESULT_CLOSED` | 結果画面が自動クローズした |
//! | `{"type":...}` | BLE (NT-100B / NBP-1BLE) の測定データ・状態。
//!   ble-medical-gateway のシリアル JSON 互換 (ble.rs 参照) |
//!
//! ログ出力 (`I (123) ...` 等) も同じコンソールに混在するため、ホスト側は
//! 既知プレフィックス (OK/ERR/PONG/STATUS/FC1200/EVT/`{`) の行のみ解釈すること。

use std::io::BufRead;
use std::sync::mpsc::Sender;

use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys;

use crate::{
    config,
    status::{now_ms, SharedStatus},
    ui::UiCommand,
};

pub fn start(tx: Sender<UiCommand>, status: SharedStatus) -> Result<()> {
    // USB Serial/JTAG ドライバを VFS に接続し、stdin のブロッキング読み出しを
    // 可能にする (CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y 前提)
    unsafe {
        let mut cfg = sys::usb_serial_jtag_driver_config_t {
            tx_buffer_size: 1024,
            rx_buffer_size: 1024,
        };
        sys::usb_serial_jtag_driver_install(&mut cfg);
        sys::esp_vfs_usb_serial_jtag_use_driver();
    }

    std::thread::Builder::new()
        .name("host_link".into())
        .stack_size(8192)
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match stdin.lock().read_line(&mut line) {
                    Ok(0) => FreeRtos::delay_ms(50),
                    Ok(_) => handle_line(line.trim(), &tx, &status),
                    Err(_) => FreeRtos::delay_ms(100),
                }
            }
        })?;
    Ok(())
}

fn handle_line(line: &str, tx: &Sender<UiCommand>, status: &SharedStatus) {
    if line.is_empty() {
        return;
    }
    let mut it = line.split_whitespace();
    let cmd = it.next().unwrap_or("").to_ascii_uppercase();
    match cmd.as_str() {
        "PING" => println!("PONG"),
        "QR" => match it.next() {
            Some(payload) => {
                let timeout_ms = it
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|s| s * 1000)
                    .unwrap_or(config::QR_DEFAULT_TIMEOUT_MS);
                let _ = tx.send(UiCommand::ShowQr {
                    payload: payload.to_string(),
                    timeout_ms,
                });
                println!("OK QR");
            }
            None => println!("ERR QR: payload がありません"),
        },
        "MEASURE" => {
            let _ = tx.send(UiCommand::Measure);
            println!("OK MEASURE");
        }
        "RESULT" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some(v @ ("OK" | "NG")) => {
                let _ = tx.send(UiCommand::Result {
                    ok: v == "OK",
                    value: it.next().unwrap_or("").to_string(),
                });
                println!("OK RESULT");
            }
            _ => println!("ERR RESULT: OK|NG が必要です"),
        },
        "ERROR" => {
            let message = line
                .splitn(2, char::is_whitespace)
                .nth(1)
                .unwrap_or("")
                .trim()
                .to_string();
            let _ = tx.send(UiCommand::Error { message });
            println!("OK ERROR");
        }
        "RESET" => {
            let _ = tx.send(UiCommand::Reset);
            println!("OK RESET");
        }
        "STATUS" => {
            let st = status.lock().map(|s| s.clone()).unwrap_or_default();
            println!(
                "STATUS LAN={} RS232={} BLE={}",
                u8::from(st.lan_link),
                u8::from(st.rs232_active(now_ms(), config::RS232_ACTIVE_WINDOW_MS)),
                u8::from(st.ble_connected),
            );
        }
        _ => println!("ERR 不明なコマンド: {cmd}"),
    }
}
