//! ホストリンク: CoreS3 ネイティブ USB-C (USB Serial/JTAG) 経由の入出力。
//!
//! 同一ストリームに 2 種類のトラフィックが流れる:
//! 1. 行指向テキストプロトコル (Windows PC / Android タブレット)
//! 2. Improv Wi-Fi Serial のバイナリフレーム (ESP Web Tools の Wi-Fi 設定)
//!
//! 受信バイト列は IMPROV マジックで振り分け、それ以外を行として解釈する。
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
//! | `ROTATE <0\|90\|180\|270>` | 画面向きを変更 (NVS 保存、次回起動も維持) |
//! | `STATUS` | `STATUS LAN=0 RS232=1 BLE=0 WIFI=0 ROT=0` を返す |
//! | `AUTH SET <id> <secret> <tenant>` | device credential を注入 (USB provisioning) |
//! | `AUTH UNPAIR` | 保存済み device credential を破棄 (ローカルのみ) |
//! | `AUTH STATUS` | `AUTH PAIRED <tenant> <id>` / `AUTH UNPAIRED` を返す |
//! | `AUTH URL <url>` | auth-worker ベース URL を上書き (staging テスト用) |
//! | `AUTH TOKEN` | device JWT 取得の自己診断 (`EVT AUTH_TOKEN ...`) |
//! | `WS URL <url>` | cf-alc-recorder WS URL を上書き (staging テスト用) |
//! | `WS STATUS` | `WS CONNECTED=1 QUEUE=3 SEQ=42` を返す |
//!
//! # 送信イベント (CoreS3 → ホスト)
//!
//! | イベント | 説明 |
//! |---|---|
//! | `FC1200 <hex>` | RS232 (FC-1200) からの受信データ (パススルー) |
//! | `EVT QR_TIMEOUT` | QR 画面が有効期限切れで閉じた |
//! | `EVT RESULT_CLOSED` | 結果画面が自動クローズした |
//! | `EVT TENKO_START` | 画面メニューから点呼が開始された |
//! | `{"type":...}` | BLE (NT-100B / NBP-1BLE) の測定データ・状態。
//!   ble-medical-gateway のシリアル JSON 互換 (ble.rs 参照) |
//!
//! ログ出力 (`I (123) ...` 等) も同じコンソールに混在するため、ホスト側は
//! 既知プレフィックス (OK/ERR/PONG/STATUS/FC1200/EVT/`{`) の行のみ解釈すること。

use std::io::Read;
use std::sync::mpsc::Sender;

use alc_hub_core::improv as improv_proto;
use alc_hub_core::cfg::DeviceConfig;
use alc_hub_core::protocol::{parse_line, HostCommand};
use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys;

use alc_hub_common::control::PairFlag;
use alc_hub_common::{
    config,
    settings::Settings,
    status::{now_ms, SharedStatus},
    ui_api::UiCommand,
};
use alc_hub_wifi::{improv::Improv, wifi::Wifi};

use crate::auth_link::AuthCommand;

/// 行としてバッファする最大長 (超えたら読み捨て — バイナリノイズ対策)
const MAX_LINE: usize = 512;

pub fn start(
    tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
    wifi: Wifi,
    pair_flag: PairFlag,
    mut improv: Improv,
    auth_tx: Sender<AuthCommand>,
) -> Result<()> {
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
        .stack_size(12 * 1024)
        .spawn(move || {
            let mut chunk = [0u8; 64];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                match std::io::stdin().lock().read(&mut chunk) {
                    Ok(0) => FreeRtos::delay_ms(20),
                    Ok(n) => {
                        acc.extend_from_slice(&chunk[..n]);
                        drain_buffer(
                            &mut acc, &tx, &status, &settings, &wifi, &pair_flag, &mut improv,
                            &auth_tx,
                        );
                    }
                    Err(_) => FreeRtos::delay_ms(100),
                }
            }
        })?;
    Ok(())
}

/// バッファ先頭から処理できる単位 (IMPROV フレーム / テキスト行) を消費する
fn drain_buffer(
    acc: &mut Vec<u8>,
    tx: &Sender<UiCommand>,
    status: &SharedStatus,
    settings: &Settings,
    wifi: &Wifi,
    pair_flag: &PairFlag,
    improv: &mut Improv,
    auth_tx: &Sender<AuthCommand>,
) {
    loop {
        if acc.is_empty() {
            return;
        }
        match improv_proto::try_parse(acc) {
            improv_proto::Frame::Packet {
                ptype,
                data,
                consumed,
            } => {
                improv.handle_packet(ptype, &data);
                acc.drain(..consumed);
            }
            improv_proto::Frame::Corrupt { consumed } => {
                acc.drain(..consumed);
            }
            improv_proto::Frame::NeedMore => return,
            improv_proto::Frame::NotImprov => {
                // テキスト行として改行まで処理
                let Some(pos) = acc.iter().position(|&b| b == b'\n' || b == b'\r') else {
                    if acc.len() > MAX_LINE {
                        acc.clear(); // 改行の来ないゴミは捨てる
                    }
                    return;
                };
                let line_bytes: Vec<u8> = acc.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]);
                handle_line(line.trim(), tx, status, settings, wifi, pair_flag, auth_tx);
            }
        }
    }
}

/// 1 行を処理する。解析は alc-hub-core::protocol (純粋・テスト済み)、
/// 副作用 (画面遷移・NVS 保存・応答出力) はここで行う。
fn handle_line(
    line: &str,
    tx: &Sender<UiCommand>,
    status: &SharedStatus,
    settings: &Settings,
    wifi: &Wifi,
    pair_flag: &PairFlag,
    auth_tx: &Sender<AuthCommand>,
) {
    let command = match parse_line(line, config::QR_DEFAULT_TIMEOUT_MS) {
        Ok(Some(command)) => command,
        Ok(None) => return, // 空行
        Err(err_response) => {
            println!("{err_response}");
            return;
        }
    };

    match command {
        HostCommand::Ping => println!("PONG"),
        HostCommand::ShowQr {
            payload,
            timeout_ms,
        } => {
            let _ = tx.send(UiCommand::ShowQr {
                payload,
                timeout_ms,
            });
            println!("OK QR");
        }
        HostCommand::Measure => {
            let _ = tx.send(UiCommand::Measure);
            println!("OK MEASURE");
        }
        HostCommand::Result { ok, value } => {
            let _ = tx.send(UiCommand::Result { ok, value });
            println!("OK RESULT");
        }
        HostCommand::ShowError { message } => {
            let _ = tx.send(UiCommand::Error { message });
            println!("OK ERROR");
        }
        HostCommand::Reset => {
            let _ = tx.send(UiCommand::Reset);
            println!("OK RESET");
        }
        HostCommand::Rotate(deg) => match settings.set_rotation(deg) {
            Ok(()) => {
                let _ = tx.send(UiCommand::Rotate(deg));
                println!("OK ROTATE {deg}");
            }
            Err(e) => {
                log::error!("host_link: rotation 保存失敗: {e:?}");
                println!("ERR ROTATE: 保存に失敗しました");
            }
        },
        HostCommand::Status => {
            let st = status.lock().map(|s| s.clone()).unwrap_or_default();
            println!(
                "STATUS LAN={} RS232={} BLE={} WIFI={} ROT={}",
                u8::from(st.lan_link),
                u8::from(st.rs232_active(now_ms(), config::RS232_ACTIVE_WINDOW_MS)),
                u8::from(st.ble_connected),
                u8::from(st.wifi_connected),
                settings.rotation(),
            );
        }
        // 設定エクスポート: 1 行 JSON を CFG プレフィックスで返す
        HostCommand::CfgGet => println!("CFG {}", settings.export().to_json()),
        // 設定インポート: パスワードは伏せて応答
        HostCommand::CfgSet { json } => match DeviceConfig::from_json(&json) {
            Ok(cfg) => match settings.apply(&cfg) {
                Ok(()) => {
                    if let Some(deg) = cfg.rotation {
                        let _ = tx.send(UiCommand::Rotate(deg));
                    }
                    println!("OK CFG");
                }
                Err(e) => {
                    log::error!("host_link: CFG 適用失敗: {e:?}");
                    println!("ERR CFG: 保存に失敗しました");
                }
            },
            Err(msg) => println!("ERR CFG: {msg}"),
        },
        // 保存済み Wi-Fi 設定での接続テスト。失敗時は原因を切り分けて返す
        HostCommand::WifiTest => match settings.wifi_credentials() {
            Some((ssid, pass)) => match wifi.connect_with_diagnosis(&ssid, &pass) {
                Ok(ip) => println!("EVT WIFI_TEST OK {ip}"),
                Err(reason) => {
                    wifi.mark_disconnected();
                    if let Ok(mut st) = status.lock() {
                        st.push_event(now_ms(), "WiFi テスト失敗");
                    }
                    println!("EVT WIFI_TEST NG {reason}");
                }
            },
            None => println!("EVT WIFI_TEST NG 保存済み Wi-Fi 設定がありません"),
        },
        // BLE 再ペアリング: ボンド消去を BLE スレッドへ依頼 (血圧計の暗号化復旧)。
        // 実際の消去と EVT PAIR_CLEARED 出力は ble タスク側で行う
        HostCommand::BlePair => {
            pair_flag.store(true, core::sync::atomic::Ordering::SeqCst);
            println!("OK PAIR");
        }
        // device credential の注入 (USB provisioning — ホストが auth-worker
        // /device/pair 系で取得した credential をそのまま渡す)。secret は
        // 応答に echo しない
        HostCommand::AuthSet {
            device_id,
            device_secret,
            tenant_id,
        } => match settings.set_device_credential(&device_id, &device_secret, &tenant_id) {
            Ok(()) => println!("OK AUTH SET"),
            Err(e) => {
                log::error!("host_link: credential 保存失敗: {e:?}");
                println!("ERR AUTH: credential の保存に失敗しました");
            }
        },
        // JWT mint (HTTP) は auth_link スレッドへ依頼し、結果は EVT AUTH_* で届く
        HostCommand::AuthToken => match auth_tx.send(AuthCommand::MintTest) {
            Ok(()) => println!("OK AUTH TOKEN"),
            Err(_) => println!("ERR AUTH: auth_link が停止しています"),
        },
        HostCommand::AuthUnpair => match settings.clear_device_credential() {
            Ok(()) => println!("OK AUTH UNPAIR"),
            Err(e) => {
                log::error!("host_link: credential 破棄失敗: {e:?}");
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
        HostCommand::AuthUrl { url } => match settings.set_auth_url(&url) {
            Ok(()) => println!("OK AUTH URL"),
            Err(e) => {
                log::error!("host_link: auth URL 保存失敗: {e:?}");
                println!("ERR AUTH: URL の保存に失敗しました");
            }
        },
        // 測定データの WS 送信 (cf-alc-recorder、ws_uplink.rs)
        HostCommand::WsUrl { url } => match settings.set_ws_url(&url) {
            Ok(()) => println!("OK WS URL"),
            Err(e) => {
                log::error!("host_link: WS URL 保存失敗: {e:?}");
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
    }
}
