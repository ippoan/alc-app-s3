//! 測定データの WebSocket 送信 (cf-alc-recorder、ippoan/alc-app-s3#21)。
//!
//! recorder スレッドから fan-out された測定 (UplinkRecord) を NVS 永続の
//! 送信キューに積み、cf-alc-recorder `/ws` へ WSS で送る。フレームの
//! 組立/解析・キュー帳簿は alc-hub-core::uplink (純粋・テスト済み)。
//!
//! - 認証: auth_link::mint_token の device JWT を WSS ハンドシェイクの
//!   Authorization ヘッダに載せる (未ペアリング時は送信しない)
//! - 冪等: 再送は同じ seq のまま。サーバ側 UNIQUE (tenant, device, seq)
//! - 電波共存: BLE (医療機器・優先) 接続中は新規接続・送信を控える。
//!   接続済みの WS は維持する (Hibernatable WS なのでサーバコストは低い)
//! - 下り: `{"type":"command"}` は `EVT WS_COMMAND <id> <payload>` として
//!   ホストへ中継し、`payload.action == "measure"` なら点呼画面を開く。
//!   受領した command には `command_result` を返す
//!
//! # ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT WS_CONNECTED` / `EVT WS_DISCONNECTED` | WS 接続状態の変化 |
//! | `EVT WS_COMMAND <id> <payload>` | 下り command を受信 |
//! | `EVT WS_DROPPED <seq>` | キュー上限で最古の未送信測定を破棄 |

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

use alc_hub_core::uplink::{
    command_action, command_result_frame, measurement_frame, parse_downlink, Downlink,
    UplinkQueue, PING_FRAME,
};
use anyhow::Result;
use esp_idf_svc::ws::client::{
    EspWebSocketClient, EspWebSocketClientConfig, FrameType, WebSocketEventType,
};

use alc_hub_common::{
    measurement::UplinkRecord,
    settings::Settings,
    status::{now_ms, SharedStatus},
    ui_api::UiCommand,
};

use crate::auth_link;

/// NVS キューの最大保持件数 (NVS 文字列 4KB 制限に収める)
const MAX_QUEUE: usize = 20;
/// 接続タイムアウト
const CONNECT_TIMEOUT_S: u64 = 10;
/// keep-alive ping の間隔
const PING_INTERVAL_MS: u64 = 30_000;
/// 未 ack エントリの再送間隔 (サーバ側で冪等なので重複送信は無害)
const RESEND_INTERVAL_MS: u64 = 15_000;
/// 接続失敗・切断時の再接続バックオフ
const RECONNECT_BACKOFF_MS: u64 = 20_000;
/// device JWT の残り有効期間がこれを切ったら再 mint
const TOKEN_REFRESH_MARGIN_S: u64 = 120;

/// WS イベントコールバック → 送信スレッドへの通知
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
    // TLS ハンドシェイクが呼び出しスレッドのスタックを使うため大きめ
    std::thread::Builder::new()
        .name("ws_uplink".into())
        .stack_size(20 * 1024)
        .spawn(move || run(rx, ui_tx, status, settings))?;
    Ok(())
}

/// 接続中の WS クライアントと付随状態
struct Conn {
    client: EspWebSocketClient<'static>,
    connected: bool,
}

fn run(
    rx: Receiver<UplinkRecord>,
    ui_tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
) {
    let (restored, skipped) =
        UplinkQueue::restore(settings.ws_last_seq(), &settings.ws_queue(), MAX_QUEUE);
    let mut queue = restored;
    if skipped > 0 {
        log::warn!("ws_uplink: NVS キューの壊れた行を {skipped} 件読み飛ばし");
    }
    publish_status(&status, &queue, false);

    let (ev_tx, ev_rx) = mpsc::channel::<WsEvent>();
    let mut conn: Option<Conn> = None;
    // device JWT と失効時刻 (稼働 ms)
    let mut token: Option<(String, u64)> = None;
    let mut backoff_until: u64 = 0;
    let mut last_ping: u64 = 0;
    let mut last_flush: u64 = 0;
    // 接続不能の連続ログを抑制する (1 回目だけ warn)
    let mut connect_warned = false;

    loop {
        // --- 1. 測定の受け取り (500ms でタイムアウトしループを回す) ---
        match rx.recv_timeout(core::time::Duration::from_millis(500)) {
            Ok(rec) => {
                enqueue(&mut queue, &settings, &rec);
                while let Ok(rec) = rx.try_recv() {
                    enqueue(&mut queue, &settings, &rec);
                }
                publish_status(&status, &queue, conn.as_ref().is_some_and(|c| c.connected));
                last_flush = 0; // 新規測定は即送信
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                log::warn!("ws_uplink: 送信元 channel が閉じたため終了");
                return;
            }
        }

        // --- 2. WS イベントの処理 ---
        let mut dirty = false;
        while let Ok(ev) = ev_rx.try_recv() {
            match ev {
                WsEvent::Connected => {
                    if let Some(c) = conn.as_mut() {
                        c.connected = true;
                    }
                    connect_warned = false;
                    println!("EVT WS_CONNECTED");
                    last_flush = 0; // 接続直後にキューを流す
                    dirty = true;
                }
                WsEvent::Disconnected => {
                    if conn.take().is_some() {
                        println!("EVT WS_DISCONNECTED");
                    }
                    backoff_until = now_ms() + RECONNECT_BACKOFF_MS;
                    dirty = true;
                }
                WsEvent::Text(text) => {
                    handle_downlink(&text, &mut queue, &settings, &mut conn, &ui_tx);
                    dirty = true;
                }
            }
        }
        if dirty {
            publish_status(&status, &queue, conn.as_ref().is_some_and(|c| c.connected));
        }

        let now = now_ms();
        let (wifi_up, ble_busy) = status
            .lock()
            .map(|s| (s.wifi_connected, s.ble_connected))
            .unwrap_or((false, false));

        // --- 3. 接続管理 ---
        // BLE 測定中は 2.4GHz を医療機器に譲る (新規接続もハンドシェイク分の
        // 電波を使うため控える)。切断は行わず既存接続は維持する
        if conn.is_none() && !queue.is_empty() && wifi_up && !ble_busy && now >= backoff_until {
            match connect(&settings, &mut token, ev_tx.clone(), now) {
                Ok(c) => conn = Some(c),
                Err(e) => {
                    if !connect_warned {
                        log::warn!("ws_uplink: 接続失敗 (バックオフ後に再試行): {e}");
                        connect_warned = true;
                    }
                    backoff_until = now + RECONNECT_BACKOFF_MS;
                }
            }
        }

        let connected = conn.as_ref().is_some_and(|c| c.connected);
        if !connected {
            continue;
        }

        // --- 4. キューの送信 (BLE 測定中は控える) ---
        if !ble_busy && !queue.is_empty() && now.saturating_sub(last_flush) >= RESEND_INTERVAL_MS {
            // 再送も同じ seq (サーバ冪等)。send 失敗は接続破棄 → 再接続
            let mut failed = false;
            {
                let c = conn.as_mut().expect("connected implies conn");
                for entry in queue.entries() {
                    match measurement_frame(entry) {
                        Ok(frame) => {
                            if let Err(e) = c.client.send(FrameType::Text(false), frame.as_bytes())
                            {
                                log::warn!("ws_uplink: 送信失敗 seq={}: {e:?}", entry.seq);
                                failed = true;
                                break;
                            }
                        }
                        Err(e) => log::error!("ws_uplink: フレーム組立失敗 seq={}: {e}", entry.seq),
                    }
                }
            }
            if failed {
                conn = None;
                backoff_until = now + RECONNECT_BACKOFF_MS;
                publish_status(&status, &queue, false);
                continue;
            }
            last_flush = now;
        }

        // --- 5. keep-alive ping (キューが空の間も下り command を受けるため) ---
        if now.saturating_sub(last_ping) >= PING_INTERVAL_MS {
            let c = conn.as_mut().expect("connected implies conn");
            if let Err(e) = c.client.send(FrameType::Text(false), PING_FRAME.as_bytes()) {
                log::warn!("ws_uplink: ping 失敗: {e:?}");
                conn = None;
                backoff_until = now + RECONNECT_BACKOFF_MS;
                publish_status(&status, &queue, false);
                continue;
            }
            last_ping = now;
        }
    }
}

/// 測定をキューへ積み NVS へ永続化する
fn enqueue(queue: &mut UplinkQueue, settings: &Settings, rec: &UplinkRecord) {
    match queue.push(rec.kind, rec.recorded_at_ms, &rec.payload) {
        Ok((_, dropped)) => {
            if let Some(seq) = dropped {
                log::warn!("ws_uplink: キュー上限で seq={seq} を破棄");
                println!("EVT WS_DROPPED {seq}");
            }
            persist(settings, queue);
        }
        Err(e) => log::error!("ws_uplink: 不正 payload を破棄: {e}"),
    }
}

fn persist(settings: &Settings, queue: &UplinkQueue) {
    settings.set_ws_last_seq(queue.last_seq());
    settings.set_ws_queue(&queue.serialize());
}

fn publish_status(status: &SharedStatus, queue: &UplinkQueue, connected: bool) {
    if let Ok(mut st) = status.lock() {
        st.ws_connected = connected;
        st.ws_queue_len = queue.len();
        st.ws_last_seq = queue.last_seq();
    }
}

/// 下りフレームの処理 (ack 消し込み / command 中継)
fn handle_downlink(
    text: &str,
    queue: &mut UplinkQueue,
    settings: &Settings,
    conn: &mut Option<Conn>,
    ui_tx: &Sender<UiCommand>,
) {
    match parse_downlink(text) {
        Ok(Downlink::Ack { seq }) => {
            if queue.ack(seq) {
                persist(settings, queue);
            }
        }
        Ok(Downlink::ServerError { seq, message }) => {
            // キューに残して次の再送周期で送り直す
            log::warn!("ws_uplink: サーバエラー seq={seq:?}: {message}");
        }
        Ok(Downlink::Command { id, payload }) => {
            println!("EVT WS_COMMAND {id} {payload}");
            // 遠隔 MEASURE 指示は点呼画面を開く (それ以外の解釈はホスト側)
            if command_action(&payload).as_deref() == Some("measure") {
                let _ = ui_tx.send(UiCommand::Measure);
            }
            if let Some(c) = conn.as_mut() {
                match command_result_frame(&id, "{}") {
                    Ok(frame) => {
                        if let Err(e) = c.client.send(FrameType::Text(false), frame.as_bytes()) {
                            log::warn!("ws_uplink: command_result 送信失敗: {e:?}");
                        }
                    }
                    Err(e) => log::error!("ws_uplink: command_result 組立失敗: {e}"),
                }
            }
        }
        Ok(Downlink::Connected) | Ok(Downlink::Pong) => {}
        Err(e) => log::warn!("ws_uplink: 下りフレーム解析失敗: {e} ({text})"),
    }
}

/// device JWT を確保し (期限切れ間近なら再 mint)、WSS 接続を開始する
fn connect(
    settings: &Settings,
    token: &mut Option<(String, u64)>,
    ev_tx: mpsc::Sender<WsEvent>,
    now: u64,
) -> Result<Conn, String> {
    let needs_mint = match token {
        Some((_, expires_at_ms)) => now + TOKEN_REFRESH_MARGIN_S * 1000 >= *expires_at_ms,
        None => true,
    };
    if needs_mint {
        let (id, secret) = settings
            .device_credential()
            .ok_or("未ペアリング (AUTH PAIR で登録してください)")?;
        let t = auth_link::mint_token(&settings.auth_url(), &id, &secret)?;
        *token = Some((t.access_token, now + t.expires_in_s * 1000));
    }
    let jwt = &token.as_ref().expect("token minted above").0;

    let headers = format!("Authorization: Bearer {jwt}\r\n");
    let config = EspWebSocketClientConfig {
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        headers: Some(&headers),
        ..Default::default()
    };
    let client = EspWebSocketClient::new(
        &settings.ws_url(),
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
            Err(e) => log::warn!("ws_uplink: WS イベントエラー: {e:?}"),
        },
    )
    .map_err(|e| format!("WS 接続開始に失敗: {e:?}"))?;

    Ok(Conn {
        client,
        connected: false,
    })
}
