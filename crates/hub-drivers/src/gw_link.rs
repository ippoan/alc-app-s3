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
//! - 接続先は **自動発見**: GW が UDP 9001 へブロードキャストする beacon
//!   (`{"src":"alc-gw","type":"beacon","ws":"ws://<GW>:9000"}`、alc-gw
//!   internal/discovery) から URL を得る。NVS の `GW URL` 設定があれば
//!   そちらを優先 (セグメント跨ぎ等の手動オーバーライド)
//!
//! # 下りコマンド (GW → CoreS3)
//!
//! - `{"type":"fc1200_command","command":"reset"}` — 点呼UI の測定開始。
//!   点呼画面を開く (UiCommand::Measure、遠隔 MEASURE と同じ挙動)
//! - `{"type":"ble_command","command":"reset"}` — BLE は常時スキャンのため
//!   何もしない (ログのみ)
//!
//! # 相互認証ハンドシェイク (ippoan/alc-app-s3#83)
//!
//! 偽ビーコンに誘導された rogue GW へ測定データを漏らさないよう、GW が
//! `{"type":"auth_challenge","nonce":...}` を送ってきたら以下を行う (旧 GW =
//! challenge を送ってこない相手には従来通り即データ送信、移行期間の後方互換):
//!
//! 1. 既存 auth_link (auth-worker への都度 TLS) で `POST /device/hub-token`
//!    (nonce 渡し) → 得た token を `{"type":"auth","token":...}` として GW へ送信
//! 2. GW の `{"type":"auth_ok","token":...}` (GW 自身の hub-token) を
//!    `POST /device/introspect` で検証 (site_id が自分の device_id と一致 かつ
//!    role=device-gateway の場合のみ許可、1:1 強制)
//! 3. 検証完了までは measurement/ble_status 等のデータフレームを送らず
//!    キューに溜める (`AUTH_QUEUE_CAP` を超えたら古いものから捨てる)。
//!    検証失敗・タイムアウト・`auth_fail` 受信は切断してビーコン再発見へ戻す
//!
//! NVS の追加は無い (credential は既存 device-hub、site_id は auth-worker 側で
//! 付与済み)。新規 TLS 接続も張らない (`auth_link` の都度 POST を流用)。
//!
//! # ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT GW_CONNECTED` / `EVT GW_DISCONNECTED` | GW 接続状態の変化 |
//! | `EVT GW_AUTH_OK` | 相互認証ハンドシェイク成功 (データ送信解禁) |
//! | `EVT GW_AUTH_FAIL <理由>` | 相互認証失敗 (切断・再発見へ) |

use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

use alc_hub_core::gw::{self, GwDownlink};
use alc_hub_core::pairing::IntrospectResult;
use anyhow::Result;
use esp_idf_svc::ws::client::{
    EspWebSocketClient, EspWebSocketClientConfig, FrameType, WebSocketEventType,
};

use crate::auth_link;
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
/// GW 自動発見 beacon の受信ポート (alc-gw internal/discovery と対)
const BEACON_PORT: u16 = 9001;
/// `auth_challenge` を受けてから `auth_ok` (または `auth_fail`) を待つ上限。
/// hub-token の TTL (60s) より十分短く、詰まった相手を早めに見限る
const AUTH_TIMEOUT_MS: u64 = 15_000;
/// 認証待ち中に溜める測定の上限。通常はハンドシェイクが数百ms〜数秒で終わる
/// ため到達しない想定 — 超えたら古いものから捨てる (無制限成長の防止)
const AUTH_QUEUE_CAP: usize = 16;

/// 相互認証ハンドシェイクの状態 (ippoan/alc-app-s3#83)。`auth_challenge` を
/// 送ってこない旧 GW は `Open` のまま動き続ける (移行期間の後方互換)。
/// 全 variant が Copy な値のみ持つため `Copy`/`Clone` を導出し、
/// `if let AuthState::AwaitingAuthOk { .. } = auth_state` の値マッチで
/// `auth_state` を move してしまわないようにする。
#[derive(Clone, Copy)]
enum AuthState {
    /// 未検証だが challenge も届いていない (旧 GW 互換) — データ送信可
    Open,
    /// `auth_challenge` を受け自分の hub-token を送信済み、GW の `auth_ok`
    /// (または `auth_fail`/timeout) 待ち — データ送信不可
    AwaitingAuthOk { deadline_ms: u64 },
    /// `auth_ok` を introspect で検証済み — データ送信可
    Verified,
}

impl AuthState {
    /// measurement/ble_status 等のデータフレームを送ってよいか。
    fn allows_data(&self) -> bool {
        !matches!(self, AuthState::AwaitingAuthOk { .. })
    }
}

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
    // 相互認証ハンドシェイクの状態 (ippoan/alc-app-s3#83)。接続のたびに Open へ
    // 戻し、その接続で GW が challenge を送ってくるかどうかで以後を決める
    let mut auth_state = AuthState::Open;
    // 認証待ち中に溜まった測定 (検証完了で flush、失敗/タイムアウトで破棄)
    let mut auth_queue: VecDeque<UplinkRecord> = VecDeque::new();

    // GW 自動発見: beacon (UDP 9001) の受信ソケット。bind 失敗しても
    // NVS の GW URL 経路は生きるので続行する
    let beacon_sock = UdpSocket::bind(("0.0.0.0", BEACON_PORT))
        .and_then(|s| s.set_nonblocking(true).map(|()| s))
        .map_err(|e| log::warn!("gw_link: beacon 受信ソケット確保失敗 (自動発見無効): {e}"))
        .ok();
    // beacon で見つけた GW の WS URL (最新のものだけ保持)
    let mut discovered: Option<String> = None;

    loop {
        // --- 1. 測定の受け取り (500ms でタイムアウトしループを回す) ---
        match rx.recv_timeout(core::time::Duration::from_millis(500)) {
            Ok(rec) => {
                relay_or_queue(&mut conn, &mut connected, &auth_state, &mut auth_queue, rec);
                while let Ok(rec) = rx.try_recv() {
                    relay_or_queue(&mut conn, &mut connected, &auth_state, &mut auth_queue, rec);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                log::warn!("gw_link: 送信元 channel が閉じたため終了");
                return;
            }
        }

        // --- 1.5 GW beacon の受信 (自動発見) ---
        if let Some(sock) = &beacon_sock {
            let mut buf = [0u8; 256];
            // 溜まっている beacon を全部読む (最新の URL だけ残す)
            while let Ok((n, _from)) = sock.recv_from(&mut buf) {
                let Ok(text) = core::str::from_utf8(&buf[..n]) else {
                    continue;
                };
                match gw::parse_beacon(text) {
                    Ok(url) => {
                        if discovered.as_deref() != Some(url.as_str()) {
                            log::info!("gw_link: beacon で GW を発見: {url}");
                            if let Ok(mut st) = status.lock() {
                                st.gw_discovered_url = url.clone();
                            }
                            discovered = Some(url);
                            // 新しい GW を見つけたら未接続時のバックオフを解除
                            if conn.is_none() {
                                backoff_until = 0;
                            }
                        }
                    }
                    Err(_) => {} // 他プロトコルのブロードキャストは黙って無視
                }
            }
        }

        // --- 2. WS イベントの処理 ---
        while let Ok(ev) = ev_rx.try_recv() {
            match ev {
                WsEvent::Connected => {
                    connected = true;
                    connect_warned = false;
                    auth_state = AuthState::Open;
                    auth_queue.clear();
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
                    auth_state = AuthState::Open;
                    auth_queue.clear();
                    backoff_until = now_ms() + RECONNECT_BACKOFF_MS;
                    publish(&status, connected);
                }
                WsEvent::Text(text) => handle_downlink(
                    &text,
                    &ui_tx,
                    &settings,
                    &mut conn,
                    &mut connected,
                    &mut auth_state,
                    &mut auth_queue,
                    &mut backoff_until,
                ),
            }
        }

        let now = now_ms();

        // --- 2.5 認証タイムアウトの確認 (auth_ok/auth_fail が来ないまま放置) ---
        if let AuthState::AwaitingAuthOk { deadline_ms } = auth_state {
            if now >= deadline_ms {
                log::warn!("gw_link: 認証タイムアウト — 切断");
                println!("EVT GW_AUTH_FAIL timeout");
                conn = None;
                connected = false;
                auth_state = AuthState::Open;
                auth_queue.clear();
                backoff_until = now + RECONNECT_BACKOFF_MS;
            }
        }

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

        // --- 3. 接続管理 ---
        // 接続先は NVS の `GW URL` (手動オーバーライド) > beacon 自動発見の順。
        // BLE 測定中は 2.4GHz を医療機器に譲る (ws_uplink と同じ判断)。
        // 切断は行わず既存接続は維持する
        if conn.is_none() && net_up && !ble_busy && now >= backoff_until {
            if let Some(url) = settings.gw_url().or_else(|| discovered.clone()) {
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
                // URL 未確定 (設定なし・beacon 未受信) の間は 5 秒おきに見る
                backoff_until = now + 5_000;
            }
        }

        // --- 4. BLE 機器状態の変化を通知 (点呼UI の 体温計/血圧計 接続表示) ---
        // 認証未完了 (AwaitingAuthOk) の間はデータフレームなので送らない
        if connected && auth_state.allows_data() {
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

/// 測定 1 件を GW へ中継する。未接続なら捨てる (記録は ws_uplink 側が担保)。
/// 認証待ち (`!auth_state.allows_data()`) の間はキューへ積み、検証完了後に
/// まとめて送る (ippoan/alc-app-s3#83、上限超過は古いものから捨てる)
fn relay_or_queue(
    conn: &mut Option<EspWebSocketClient<'static>>,
    connected: &mut bool,
    auth_state: &AuthState,
    queue: &mut VecDeque<UplinkRecord>,
    rec: UplinkRecord,
) {
    if !*connected {
        log::info!("gw_link: 未接続のため {} を中継せず破棄", rec.kind);
        return;
    }
    if !auth_state.allows_data() {
        if queue.len() >= AUTH_QUEUE_CAP {
            queue.pop_front();
        }
        queue.push_back(rec);
        return;
    }
    relay(conn, connected, &rec);
}

/// 測定 1 件を GW へ中継する (認証済み/旧GW互換の即時送信)。
fn relay(conn: &mut Option<EspWebSocketClient<'static>>, connected: &mut bool, rec: &UplinkRecord) {
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

/// GW からの下りフレームの処理 (相互認証ハンドシェイクを含む、ippoan/alc-app-s3#83)。
#[allow(clippy::too_many_arguments)]
fn handle_downlink(
    text: &str,
    ui_tx: &Sender<UiCommand>,
    settings: &Settings,
    conn: &mut Option<EspWebSocketClient<'static>>,
    connected: &mut bool,
    auth_state: &mut AuthState,
    auth_queue: &mut VecDeque<UplinkRecord>,
    backoff_until: &mut u64,
) {
    match gw::parse_downlink(text) {
        Ok(GwDownlink::AuthChallenge(nonce)) => {
            *auth_state = handle_auth_challenge(&nonce, settings, conn, connected);
            if !*connected {
                *backoff_until = now_ms() + RECONNECT_BACKOFF_MS;
            }
        }
        Ok(GwDownlink::AuthOk(token)) => {
            if !matches!(auth_state, AuthState::AwaitingAuthOk { .. }) {
                log::warn!("gw_link: auth_ok を認証待ちでない状態で受信、無視");
                return;
            }
            *auth_state = handle_auth_ok(&token, settings, conn, connected);
            if *connected && auth_state.allows_data() {
                println!("EVT GW_AUTH_OK");
                while let Some(rec) = auth_queue.pop_front() {
                    relay(conn, connected, &rec);
                }
            } else if !*connected {
                println!("EVT GW_AUTH_FAIL introspect");
                *backoff_until = now_ms() + RECONNECT_BACKOFF_MS;
            }
        }
        Ok(GwDownlink::AuthFail(reason)) => {
            if !matches!(auth_state, AuthState::AwaitingAuthOk { .. }) {
                log::warn!("gw_link: auth_fail を認証待ちでない状態で受信、無視: {reason}");
                return;
            }
            log::warn!("gw_link: GW から認証失敗通知のため切断: {reason}");
            println!("EVT GW_AUTH_FAIL {reason}");
            *conn = None;
            *connected = false;
            *auth_state = AuthState::Open;
            auth_queue.clear();
            *backoff_until = now_ms() + RECONNECT_BACKOFF_MS;
        }
        // 点呼UI の測定開始 (useFc1200Serial の startMeasurement) は
        // "reset" として届く。遠隔 MEASURE と同じく点呼画面を開く。
        // 認証未完了 (旧GW以外で challenge 受信済みだが未検証) の間は無視する
        Ok(GwDownlink::Fc1200Command(cmd)) => {
            if !auth_state.allows_data() {
                log::warn!("gw_link: 認証未完了のため fc1200_command を無視: {cmd}");
            } else if cmd == "reset" {
                let _ = ui_tx.send(UiCommand::Measure);
            } else {
                log::info!("gw_link: 未対応の fc1200_command: {cmd}");
            }
        }
        // BLE は常時スキャンで運用しているため再スキャン指示は不要
        Ok(GwDownlink::BleCommand(cmd)) => {
            if !auth_state.allows_data() {
                log::warn!("gw_link: 認証未完了のため ble_command を無視: {cmd}");
            } else {
                log::info!("gw_link: ble_command {cmd} は無視");
            }
        }
        Err(e) => log::warn!("gw_link: 下りフレーム解析失敗: {e} ({text})"),
    }
}

/// `auth_challenge` の nonce で自分の hub-token を mint し `auth` フレームで
/// 送る。mint 失敗・送信失敗はどちらも切断扱い (`AuthState::Open` を返しつつ
/// `conn`/`connected` を破棄済み — 呼び出し側は `!*connected` でバックオフする)。
fn handle_auth_challenge(
    nonce: &str,
    settings: &Settings,
    conn: &mut Option<EspWebSocketClient<'static>>,
    connected: &mut bool,
) -> AuthState {
    let Some((id, secret)) = settings.device_credential() else {
        log::warn!("gw_link: auth_challenge を受けたが credential 未登録のため切断");
        *conn = None;
        *connected = false;
        return AuthState::Open;
    };
    match auth_link::hub_token(&settings.auth_url(), &id, &secret, nonce) {
        Ok(t) => {
            send(conn, connected, &gw::auth_frame(&t.access_token));
            if *connected {
                AuthState::AwaitingAuthOk {
                    deadline_ms: now_ms() + AUTH_TIMEOUT_MS,
                }
            } else {
                AuthState::Open
            }
        }
        Err(e) => {
            log::warn!("gw_link: hub-token mint 失敗のため切断: {e}");
            *conn = None;
            *connected = false;
            AuthState::Open
        }
    }
}

/// GW の `auth_ok` token を introspect で検証する。自分の site_id
/// (= 自分の device_id、ippoan/auth-worker#406) と一致し role=device-gateway の
/// 場合のみ `Verified`。それ以外・エラーは切断して `Open` へ戻す
fn handle_auth_ok(
    token: &str,
    settings: &Settings,
    conn: &mut Option<EspWebSocketClient<'static>>,
    connected: &mut bool,
) -> AuthState {
    let Some((id, secret)) = settings.device_credential() else {
        *conn = None;
        *connected = false;
        return AuthState::Open;
    };
    let result: Result<IntrospectResult, String> =
        auth_link::introspect(&settings.auth_url(), &id, &secret, token);
    match result {
        Ok(r) if r.authorizes_gateway(&id) => AuthState::Verified,
        Ok(_) => {
            log::warn!("gw_link: GW の認証トークンが無効 (site_id/role 不一致等) — 切断");
            *conn = None;
            *connected = false;
            AuthState::Open
        }
        Err(e) => {
            log::warn!("gw_link: introspect 失敗のため切断: {e}");
            *conn = None;
            *connected = false;
            AuthState::Open
        }
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
