//! Windows GW (ippoan/alc-gw) との LAN 内 WS 接続 (alc-app#120)。
//!
//! GW がサーバー (ws://<GW-IP>:9000)・CoreS3 がクライアント。recorder から
//! fan-out された測定 (UplinkRecord) を生中継し、GW はそれを点呼UI (alc-app)
//! のブリッジ互換 WS (NFC/体温血圧/FC-1200) へ流す。フレームの組立/解析は
//! alc-hub-core::gw (純粋・テスト済み)。
//!
//! ws_uplink (cf-alc-recorder 宛) との違い:
//! - LAN 内 ws:// のみ想定 = TLS も device JWT も不要 (heap 圧迫が小さい)
//! - 送信キューを持たない生中継 — GW が落ちている間の測定は捨てる
//!   (記録の永続化は ws_uplink → cf-alc-recorder が担う。GW は表示用)
//! - URL は NVS の `GW URL` 設定 (未設定なら本リンクは何もしない)
//!
//! # 下りコマンド (GW → CoreS3)
//!
//! - `{"type":"fc1200_command","command":"reset"}` — 点呼UI の測定開始。
//!   点呼画面を開く (UiCommand::Measure、遠隔 MEASURE と同じ挙動)
//! - `{"type":"ble_command","command":"reset"}` — BLE は常時スキャンのため
//!   何もしない (ログのみ)
//!
//! # ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT GW_CONNECTED` / `EVT GW_DISCONNECTED` | GW 接続状態の変化 |

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

use alc_hub_core::gw::{self, GwDownlink};
use anyhow::Result;
use esp_idf_svc::ws::client::{
    EspWebSocketClient, EspWebSocketClientConfig, FrameType, WebSocketEventType,
};

use alc_hub_common::{
    config,
    measurement::UplinkRecord,
    settings::Settings,
    status::{now_ms, SharedStatus},
    ui_api::UiCommand,
};

/// 接続タイムアウト
const CONNECT_TIMEOUT_S: u64 = 10;
/// 接続失敗・切断時の再接続バックオフ
const RECONNECT_BACKOFF_MS: u64 = 10_000;

/// WS イベントコールバック → 本スレッドへの通知
enum WsEvent {
    Connected,
    Disconnected,
    Text(String),
}

pub fn start(
    rx: Receiver<UplinkRecord>,
    ui_tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
) -> Result<()> {
    std::thread::Builder::new()
        .name("gw_link".into())
        .stack_size(12 * 1024)
        .spawn(move || run(rx, ui_tx, status, settings))?;
    Ok(())
}

fn run(
    rx: Receiver<UplinkRecord>,
    ui_tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
) {
    let (ev_tx, ev_rx) = mpsc::channel::<WsEvent>();
    let mut conn: Option<EspWebSocketClient<'static>> = None;
    let mut connected = false;
    let mut backoff_until: u64 = 0;
    // 直近に送った BLE 機器状態 (変化時のみ ble_status を送る)
    let mut last_ble: Option<(bool, String)> = None;
    // 接続不能の連続ログを抑制する (1 回目だけ warn)
    let mut connect_warned = false;

    loop {
        // --- 1. 測定の受け取り (500ms でタイムアウトしループを回す) ---
        match rx.recv_timeout(core::time::Duration::from_millis(500)) {
            Ok(rec) => {
                relay(&mut conn, &mut connected, &rec);
                while let Ok(rec) = rx.try_recv() {
                    relay(&mut conn, &mut connected, &rec);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                log::warn!("gw_link: 送信元 channel が閉じたため終了");
                return;
            }
        }

        // --- 2. WS イベントの処理 ---
        while let Ok(ev) = ev_rx.try_recv() {
            match ev {
                WsEvent::Connected => {
                    connected = true;
                    connect_warned = false;
                    println!("EVT GW_CONNECTED");
                    // 自己紹介 (GW の readers 表示がデバイス名になる) と
                    // 現在の BLE 機器状態を送り直す
                    let device = settings
                        .device_credential()
                        .map(|(id, _)| id)
                        .unwrap_or_else(|| "cores3".to_string());
                    let hello = gw::hello_frame(&device, &config::firmware_version_full());
                    send(&mut conn, &mut connected, &hello);
                    last_ble = None;
                    publish(&status, connected);
                }
                WsEvent::Disconnected => {
                    if conn.take().is_some() {
                        println!("EVT GW_DISCONNECTED");
                    }
                    connected = false;
                    backoff_until = now_ms() + RECONNECT_BACKOFF_MS;
                    publish(&status, connected);
                }
                WsEvent::Text(text) => handle_downlink(&text, &ui_tx),
            }
        }

        let now = now_ms();
        let (net_up, ble_busy, ble_device) = status
            .lock()
            .map(|s| {
                (
                    s.wifi_connected || s.lan_link,
                    s.ble_connected,
                    s.ble_device.clone(),
                )
            })
            .unwrap_or((false, false, String::new()));

        // --- 3. 接続管理 (URL 未設定なら何もしない) ---
        // BLE 測定中は 2.4GHz を医療機器に譲る (ws_uplink と同じ判断)。
        // 切断は行わず既存接続は維持する
        if conn.is_none() && net_up && !ble_busy && now >= backoff_until {
            if let Some(url) = settings.gw_url() {
                match connect(&url, ev_tx.clone()) {
                    Ok(c) => conn = Some(c),
                    Err(e) => {
                        if !connect_warned {
                            log::warn!("gw_link: 接続失敗 (バックオフ後に再試行): {e}");
                            connect_warned = true;
                        }
                        backoff_until = now + RECONNECT_BACKOFF_MS;
                    }
                }
            } else {
                // 未設定の間は設定を 5 秒おきに見るだけ
                backoff_until = now + 5_000;
            }
        }

        // --- 4. BLE 機器状態の変化を通知 (点呼UI の 体温計/血圧計 接続表示) ---
        if connected {
            let cur = (ble_busy, ble_device);
            if last_ble.as_ref() != Some(&cur) {
                let frame = gw::ble_status_frame(cur.0, &cur.1);
                send(&mut conn, &mut connected, &frame);
                if connected {
                    last_ble = Some(cur);
                }
            }
        }
    }
}

/// 測定 1 件を GW へ中継する。未接続なら捨てる (記録は ws_uplink 側が担保)
fn relay(conn: &mut Option<EspWebSocketClient<'static>>, connected: &mut bool, rec: &UplinkRecord) {
    if !*connected {
        log::info!("gw_link: 未接続のため {} を中継せず破棄", rec.kind);
        return;
    }
    match gw::measurement_frame(rec.kind, &rec.payload) {
        Ok(frame) => send(conn, connected, &frame),
        Err(e) => log::error!("gw_link: フレーム組立失敗 ({}): {e}", rec.kind),
    }
}

/// テキストフレームを送る。失敗したら接続を破棄する (次周期で再接続)
fn send(conn: &mut Option<EspWebSocketClient<'static>>, connected: &mut bool, frame: &str) {
    let Some(c) = conn.as_mut() else { return };
    if let Err(e) = c.send(FrameType::Text(false), frame.as_bytes()) {
        log::warn!("gw_link: 送信失敗: {e:?}");
        *conn = None;
        *connected = false;
    }
}

/// GW からの下りコマンドの処理
fn handle_downlink(text: &str, ui_tx: &Sender<UiCommand>) {
    match gw::parse_downlink(text) {
        // 点呼UI の測定開始 (useFc1200Serial の startMeasurement) は
        // "reset" として届く。遠隔 MEASURE と同じく点呼画面を開く
        Ok(GwDownlink::Fc1200Command(cmd)) if cmd == "reset" => {
            let _ = ui_tx.send(UiCommand::Measure);
        }
        Ok(GwDownlink::Fc1200Command(cmd)) => {
            log::info!("gw_link: 未対応の fc1200_command: {cmd}");
        }
        // BLE は常時スキャンで運用しているため再スキャン指示は不要
        Ok(GwDownlink::BleCommand(cmd)) => log::info!("gw_link: ble_command {cmd} は無視"),
        Err(e) => log::warn!("gw_link: 下りフレーム解析失敗: {e} ({text})"),
    }
}

fn publish(status: &SharedStatus, connected: bool) {
    if let Ok(mut st) = status.lock() {
        st.gw_connected = connected;
    }
}

/// GW ハブへの WS 接続を開始する (LAN 内 ws:// 前提、認証なし)
fn connect(url: &str, ev_tx: mpsc::Sender<WsEvent>) -> Result<EspWebSocketClient<'static>, String> {
    let config = EspWebSocketClientConfig {
        ..Default::default()
    };
    EspWebSocketClient::new(
        url,
        &config,
        core::time::Duration::from_secs(CONNECT_TIMEOUT_S),
        move |event| match event {
            Ok(ev) => match &ev.event_type {
                WebSocketEventType::Connected => {
                    let _ = ev_tx.send(WsEvent::Connected);
                }
                WebSocketEventType::Disconnected
                | WebSocketEventType::Close(_)
                | WebSocketEventType::Closed => {
                    let _ = ev_tx.send(WsEvent::Disconnected);
                }
                WebSocketEventType::Text(text) => {
                    let _ = ev_tx.send(WsEvent::Text((*text).to_string()));
                }
                _ => {}
            },
            Err(e) => log::warn!("gw_link: WS イベントエラー: {e:?}"),
        },
    )
    .map_err(|e| format!("WS 接続開始に失敗: {e:?}"))
}
