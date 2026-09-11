//! 測定データの WebSocket 送信 (cf-alc-recorder、ippoan/alc-app-s3#21)。
//!
//! recorder スレッドから fan-out された測定 (UplinkRecord) を NVS 永続の
//! 送信キューに積み、cf-alc-recorder `/ws` へ WSS で送る。フレームの
//! 組立/解析・キュー帳簿は alc-hub-core::uplink (純粋・テスト済み)。
//!
//! 接続はキューが空でも張りっぱなし (常時接続、Refs #25) — WS を選んだ理由で
//! ある下り push (timecard / 遠隔 MEASURE) をいつでも受けられるようにする。
//! PSRAM 有効化により TLS が内蔵 SRAM を圧迫しなくなったため成立する。
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
//! | `EVT WS_COMMAND <id> <payload>` | 下り command を受信。`get_log` のときはキオスク PWA が `PWALOG <id> …` を返す合図を兼ねる (#215、pwalog.rs) |
//! | `EVT WS_DROPPED <seq> <kind>` | 保存先が一杯で最古の未送信測定を破棄 |
//! | `EVT PUNCHQ <mode> count=<n>` | 送信キューの保存先と未送信件数 (punchq.rs) |
//! | `EVT OTA_ROLLED_BACK free_int=<n> min_int=<n> reason=<語>` | 前の起動で OTA 直後の image を戻した (戻った先の起動で出る。Refs #217) |
//! | `EVT OTA_ROLLBACK_UNAVAILABLE` | 戻そうとしたが戻し先が無い — この image を確定して続ける |

use std::io::Write;
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

use alc_hub_core::uplink::{
    command_action, command_gw_url, command_log_max_bytes, command_ota_url,
    command_print_chunk, command_print_url, command_result_frame, measurement_frame, ota_guard,
    parse_downlink, should_wait_for_clock, Downlink, DroppedEntry, OtaGuard, UplinkQueue,
    OTA_VERIFY_TIMEOUT_MS, PING_FRAME,
};
use anyhow::Result;
use esp_idf_svc::ws::client::{
    EspWebSocketClient, EspWebSocketClientConfig, FrameType, WebSocketEventType,
};

use alc_hub_common::{
    measurement::UplinkRecord,
    settings::Settings,
    status::{epoch_ms, now_ms, SharedStatus},
    ui_api::UiCommand,
};

use crate::{auth_link, punchq};

/// 送信窓 = RAM に載せる未 ack エントリの件数 (Refs #142)。
/// **保持できる総件数はここではなく保存先 (punchq パーティション) の容量で決まる**
/// — flash がキューの本体で、窓が空いたら次を読み込む。RAM 使用量は総件数に
/// 依存しないので、PSRAM の有無を見る必要も無い
const WINDOW: usize = 20;
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

/// 切断のまま自動再接続が来ない場合に端末を再起動するまでの時間。
///
/// client を drop して作り直せないため (esp-idf-svc の Drop が切断済み client で
/// panic する)、復帰手段は再起動しかない。**panic による再起動と違い、これは
/// クリーンな再起動**で crash_log も出ない。キューは NVS 永続なので測定は失わない。
const WS_STALE_RESTART_MS: u64 = 5 * 60 * 1000;
/// TLS ハンドシェイク (mint + WSS) を始めるのに必要な空きヒープ。
/// BLE (NimBLE) と同時にヒープを食い合うと BLE 側が Malloc failed で
/// 測定不能になる (実機で確認) ため、余裕がない間は接続を延期する。
/// 実測: Wi-Fi + BLE + UI 起動後の定常空きは約 70KB (バッファ削減後)、
/// TLS ハンドシェイクのピークは DYNAMIC_BUFFER 有効で約 30KB
///
/// NFC (nfc-verify) を積むと定常空きが下がりこの値を割るが、**ゲートは
/// 下げない**。下げると BLE 側が Malloc failed で測定不能になる条件へ
/// 近づくだけで、原因 (内部RAM の食い過ぎ) は残る。実空きを増やして
/// 満たすこと — スタック削減 (下記) と `CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL`
/// の引き下げが効く。
const MIN_FREE_HEAP_FOR_TLS: u32 = 60 * 1024;

/// WS イベントコールバック → 送信スレッドへの通知
enum WsEvent {
    Connected,
    Disconnected,
    Text(String),
    /// 他スレッド (OTA 進捗など) から本ループ経由で送出したいフレーム。
    /// WS client はスレッド安全でないため、送信は必ずこのループに集約する。
    Outbound(String),
}

pub fn start(
    rx: Receiver<UplinkRecord>,
    ui_tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
    boot_id: u32,
) -> Result<()> {
    // 前の起動で OTA 直後の image を戻していたら、その証跡を出して消す (Refs #217)。
    // 戻った先がこのコードを持つ image のときだけ読まれる
    if let Some(note) = settings.take_ota_rollback_note() {
        alc_hub_common::evtlog::emit(&format!("EVT OTA_ROLLED_BACK {note}"));
    }
    // TLS ハンドシェイクが呼び出しスレッドのスタックを使うため大きめ
    crate::task::name_next(c"ws_uplink");
    std::thread::Builder::new()
        .name("ws_uplink".into())
        .stack_size(20 * 1024)
        .spawn(move || run(rx, ui_tx, status, settings, boot_id))?;
    Ok(())
}

/// 接続中の WS クライアントと付随状態
struct Conn {
    client: EspWebSocketClient<'static>,
    connected: bool,
    /// 切断を検知した時刻 [ms]。自動再接続が効かないまま放置されるのを
    /// 見張るために持つ (`WS_STALE_RESTART_MS`)。
    disconnected_at: Option<u64>,
    /// WS 接続が成立した時刻 [ms]。破棄ログに「どれだけ保ったか」を出すために持つ
    /// — 即切れ (認証・経路の問題) と長時間後の切断 (hibernation・アイドル) は
    /// 原因が別なので、ログだけで見分けられるようにする。None = 未成立。
    connected_at: Option<u64>,
}

/// WS push 印刷 (#38) の TcpStream 書き込みタイムアウト。fetch_and_send と同値
const PRINT_IO_TIMEOUT_S: u64 = 30;

/// WS push 印刷 (#38) の進行中セッション。`print_begin` で NVS の printer_addr
/// (9100) へ接続し、`print_data` チャンクを書き足し、`print_end` で flush して
/// 閉じる。複数の下りフレームに跨って TcpStream を保持するため、run ループが
/// `Option<PrintSession>` として所有する (Conn と同じ持ち方)。WS 切断時は破棄。
struct PrintSession {
    printer: TcpStream,
    bytes: usize,
}

impl PrintSession {
    /// printer_addr (9100) へ接続してセッションを開く
    fn open(addr: &str) -> std::io::Result<Self> {
        let printer = TcpStream::connect(addr)?;
        printer.set_write_timeout(Some(core::time::Duration::from_secs(PRINT_IO_TIMEOUT_S)))?;
        Ok(Self { printer, bytes: 0 })
    }

    /// デコード済みチャンクを 9100 へ書き足す
    fn write_chunk(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.printer.write_all(data)?;
        self.bytes += data.len();
        Ok(())
    }

    /// flush してセッションを閉じる (drop で TcpStream が close され 9100 raw は
    /// それを送信完了として扱う)
    fn finish(mut self) -> std::io::Result<()> {
        self.printer.flush()
    }
}

fn run(
    rx: Receiver<UplinkRecord>,
    ui_tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
    boot_id: u32,
) {
    // 保存先は専用 NVS パーティション punchq。無い機 (OTA だけで更新した機) は
    // 既定 nvs の文字列へフォールバックする (punchq::open_store)
    let (store, mode) = punchq::open_store(&settings);
    let (restored, skipped) = UplinkQueue::open(settings.ws_last_seq(), store, WINDOW);
    let mut queue = restored;
    log::info!(
        "ws_uplink: 送信キュー = {mode} (未送信 {} 件、窓 {WINDOW} 件)",
        queue.len()
    );
    if skipped > 0 {
        log::warn!("ws_uplink: 保存先の壊れた行を {skipped} 件読み飛ばし");
    }
    publish_status(&status, &queue, false);

    let (ev_tx, ev_rx) = mpsc::channel::<WsEvent>();
    let mut conn: Option<Conn> = None;
    // 時計未同期による送信待機を 1 回だけログする (待機中は毎周期ここを通る)
    let mut clock_wait_logged = false;
    // WS push 印刷 (#38) の進行中セッション (print_begin〜print_end)。フレーム
    // 跨ぎで 9100 の TcpStream を保持する。WS 切断で破棄する (未完印刷は中断)
    let mut print_session: Option<PrintSession> = None;
    // device JWT と失効時刻 (稼働 ms)
    let mut token: Option<(String, u64)> = None;
    let mut backoff_until: u64 = 0;
    let mut last_ping: u64 = 0;
    let mut last_flush: u64 = 0;
    // ack で窓に次のぶんが載った → 次の周回で未送信ぶんだけ即座に送る
    let mut send_unsent = false;
    // 接続不能の連続ログを抑制する (1 回目だけ warn)
    let mut connect_warned = false;
    // ヒープ不足ログの最終出力時刻
    let mut heap_log_at: u64 = 0;
    // OTA 直後の未確定状態 (PENDING_VERIFY または NEW) か。確定するか戻すまで
    // true (Refs #217)
    let mut ota_pending = crate::ota::running_app_pending();
    // この起動で一度でも WS が繋がったか
    let mut ws_ever_connected = false;
    // 接続がどの条件で止まっているか (OTA を戻すときの証跡に書く)
    let mut stall: &'static str = "not_tried";

    loop {
        // --- 1. 測定の受け取り (500ms でタイムアウトしループを回す) ---
        match rx.recv_timeout(core::time::Duration::from_millis(500)) {
            Ok(rec) => {
                enqueue(&mut queue, &settings, &rec, boot_id);
                while let Ok(rec) = rx.try_recv() {
                    enqueue(&mut queue, &settings, &rec, boot_id);
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
                        c.connected_at = Some(now_ms());
                        c.disconnected_at = None;
                    }
                    connect_warned = false;
                    alc_hub_common::evtlog::emit("EVT WS_CONNECTED");
                    // 認証付きの WS が繋がった = この image は健康。OTA 直後なら確定する
                    // (ippoan/alc-gw-p4 の ota_link_confirm_running_app と同じ位置、Refs #217)
                    ws_ever_connected = true;
                    if ota_pending {
                        crate::ota::confirm_running_app_if_pending();
                        ota_pending = false;
                    }
                    // 前の接続で送った分がサーバに届いたかは分からないので、
                    // 送信済みの印を落として窓の全件を送り直す
                    queue.reset_sent();
                    last_flush = 0; // 接続直後にキューを流す
                    dirty = true;
                }
                WsEvent::Disconnected => {
                    if mark_disconnected(&mut conn, "サーバ側から切断", &queue) {
                        alc_hub_common::evtlog::emit("EVT WS_DISCONNECTED");
                    }
                    // 印刷中の切断は未完なので破棄 (drop で 9100 を閉じる)
                    print_session = None;
                    backoff_until = now_ms() + RECONNECT_BACKOFF_MS;
                    dirty = true;
                }
                WsEvent::Text(text) => {
                    // ack で窓に次のぶんが載ったら再送周期 (15 秒) を待たずに送る。
                    // 待つと保存先に溜まった分の排出が「窓 20 件 / 15 秒」に
                    // 律速される (2,000 件で 25 分かかる、Refs #142)
                    if handle_downlink(
                        &text,
                        &mut queue,
                        &settings,
                        &mut conn,
                        &mut print_session,
                        &ui_tx,
                        &status,
                        &ev_tx,
                    ) {
                        send_unsent = true;
                    }
                    dirty = true;
                }
                WsEvent::Outbound(frame) => {
                    // OTA 進捗などの外部フレーム。接続が生きていれば送るだけ
                    // (失敗しても接続破棄はしない — 進捗は best-effort)
                    if let Some(c) = conn.as_mut() {
                        if let Err(e) = c.client.send(FrameType::Text(false), frame.as_bytes()) {
                            log::warn!("ws_uplink: outbound 送信失敗: {e:?}");
                        }
                    }
                }
            }
        }
        if dirty {
            publish_status(&status, &queue, conn.as_ref().is_some_and(|c| c.connected));
        }

        let now = now_ms();
        let (net_up, ble_busy, has_ip) = status
            .lock()
            .map(|s| {
                (
                    s.wifi_connected || s.lan_link,
                    s.ble_connected,
                    !s.wifi_ip.is_empty() || !s.lan_ip.is_empty(),
                )
            })
            .unwrap_or((false, false, false));

        // --- OTA 直後の image を確定するか戻すか (Refs #217) ---
        // 判定は hub-core の ota_guard。未登録の機は WS で判定できないのですぐ確定、
        // 登録済みで IP があるのに 10 分繋がらなければ前の image へ戻す
        if ota_pending {
            let paired = settings.device_credential().is_some();
            let confirm = match ota_guard(ota_pending, paired, has_ip, ws_ever_connected, now) {
                OtaGuard::Nothing => false,
                OtaGuard::Confirm => true,
                // 戻せれば再起動して戻らない。戻ってきたら (戻し先が無い /
                // 既に確定済み) 再起動ループを避けて確定する
                OtaGuard::Rollback => {
                    rollback_ota(&settings, &queue, stall);
                    true
                }
            };
            if confirm {
                crate::ota::confirm_running_app_if_pending();
                ota_pending = false;
            }
        }

        // --- 3. 接続管理 ---
        // キューが空でも接続を張り、下り command (timecard / 遠隔 MEASURE /
        // 印刷ブリッジの print・ota) を待ち受ける常時接続 (Refs #25。PSRAM
        // 有効化で TLS ヒープの内蔵 SRAM 圧迫が解消したため、Wi-Fi でも
        // 常設できる)。ネットワークは Wi-Fi (CoreS3) と LAN (AtomS3 印刷
        // ブリッジ = W5500、lan_link) のどちらでもよい。
        // BLE 測定中は 2.4GHz を医療機器に譲る (新規接続もハンドシェイク分の
        // 電波を使うため控える)。切断は行わず既存接続は維持する。
        // 空きヒープが少ない間も延期する (TLS と BLE のヒープ食い合い対策)
        // 自動再接続が来ないまま放置されていないか見張る。
        // client を捨てて張り直す手が使えない以上、復帰手段は再起動しかない。
        if let Some(at) = conn.as_ref().and_then(|c| (!c.connected).then_some(c.disconnected_at)).flatten() {
            if now.saturating_sub(at) > WS_STALE_RESTART_MS {
                let line = format!(
                    "ws_uplink: 自動再接続が {}分来ないため再起動します queue={}",
                    WS_STALE_RESTART_MS / 60_000,
                    queue.len()
                );
                log::warn!("{line}");
                crate::crashlog::note(&line);
                alc_hub_common::evtlog::emit("EVT WS_STALE_RESTART");
                settings.set_ws_last_seq(queue.last_seq());
                std::thread::sleep(core::time::Duration::from_millis(300));
                unsafe { esp_idf_svc::sys::esp_restart() };
            }
        }

        if conn.is_none() && net_up && !ble_busy && now >= backoff_until {
            if !heap_headroom_ok(now, &mut heap_log_at) {
                stall = "heap_gate";
            } else {
                match connect(&settings, &mut token, ev_tx.clone(), now) {
                    Ok(c) => {
                        conn = Some(c);
                        // 接続を始めた。Connected が来なければここで止まっている
                        stall = "no_answer";
                    }
                    Err(e) => {
                        if !connect_warned {
                            log::warn!("ws_uplink: 接続失敗 (バックオフ後に再試行): {e}");
                            connect_warned = true;
                        }
                        backoff_until = now + RECONNECT_BACKOFF_MS;
                        stall = "connect_err";
                    }
                }
            }
        }

        let connected = conn.as_ref().is_some_and(|c| c.connected);
        if !connected {
            continue;
        }

        // --- 4. キューの送信 (BLE 測定中は控える) ---
        // 時計がまだ未同期 (WS が NTP より先に繋がった直後) なら、補正できる測定が
        // ある間は CLOCK_WAIT_MS まで送信を待つ。先に送ると 1970 起点の時刻で
        // サーバに固定され、直す機会を失う (実測: 起動 23 秒の体温がそうなった)。
        // NTP が塞がれている環境では超過後に未同期のまま送る (測定を止めない)
        let connected_for = conn
            .as_ref()
            .and_then(|c| c.connected_at)
            .map_or(0, |at| now.saturating_sub(at));
        let wait_clock = should_wait_for_clock(epoch_ms(), connected_for, queue.has_correctable(boot_id));
        if wait_clock && !clock_wait_logged {
            log::info!("ws_uplink: 時計未同期のため送信を待機 (NTP 同期後に時刻補正して送る)");
            alc_hub_common::evtlog::emit("EVT WS_CLOCK_WAIT");
            clock_wait_logged = true;
        }
        if !wait_clock {
            clock_wait_logged = false;
        }
        // 定期の再送周期か、ack で窓に次のぶんが載った直後 (即時送信) に送る。
        // **即時送信は未送信ぶんだけ** — 窓の全件を送ると ack 1 件ごとに
        // まだ ack 待ちの最大 WINDOW-1 件も送り直すことになる (Refs #142)
        let periodic = now.saturating_sub(last_flush) >= RESEND_INTERVAL_MS;
        if !wait_clock && !ble_busy && !queue.is_empty() && (periodic || send_unsent) {
            // NTP 未同期 (ネットワーク無し) で記録した測定は recorded_at_ms が 1970 起点
            // になっている。今は同期済み (接続できている = ネットワークがある) なので、
            // 記録時と今の稼働時間の差で実時刻へ直してから送る (同じ起動の分だけ)
            let fixed = queue.fix_unsynced_times(epoch_ms(), now, boot_id);
            if fixed > 0 {
                log::info!("ws_uplink: 未同期時刻の測定 {fixed} 件を実時刻へ補正");
                alc_hub_common::evtlog::emit(&format!("EVT WS_TIME_FIXED {fixed}"));
                persist(&settings, &queue);
            }
            send_unsent = false;
            // 再送も同じ seq (サーバ冪等)。send 失敗は接続破棄 → 再接続
            if flush_queue(&mut conn, &mut queue, !periodic) {
                mark_disconnected(&mut conn, "測定の送信失敗", &queue);
                backoff_until = now + RECONNECT_BACKOFF_MS;
                publish_status(&status, &queue, false);
                continue;
            }
            if periodic {
                last_flush = now;
            }
        }

        // --- 5. keep-alive ping (キューが空の間も下り command を受けるため) ---
        if now.saturating_sub(last_ping) >= PING_INTERVAL_MS {
            // 借用を send の間だけに閉じる (この後 mark_disconnected が &mut conn を取る)
            let sent = {
                let c = conn.as_mut().expect("connected implies conn");
                c.client.send(FrameType::Text(false), PING_FRAME.as_bytes())
            };
            if let Err(e) = sent {
                log::warn!("ws_uplink: ping 失敗: {e:?}");
                mark_disconnected(&mut conn, "keep-alive ping の失敗", &queue);
                backoff_until = now + RECONNECT_BACKOFF_MS;
                publish_status(&status, &queue, false);
                continue;
            }
            last_ping = now;
        }
    }
}

/// TLS ハンドシェイクを始められるだけの空きヒープがあるか。
/// 不足ログは 30 秒に 1 回に抑える (500ms ループから毎回出さない)。
/// `esp_get_free_heap_size` は PSRAM 有効化 (#29) 後は PSRAM の空きが混ざり
/// ガードが素通りするため、内部RAM 専用に測る (Refs #27)
fn heap_headroom_ok(now: u64, last_log: &mut u64) -> bool {
    let free = unsafe {
        esp_idf_svc::sys::heap_caps_get_free_size(esp_idf_svc::sys::MALLOC_CAP_INTERNAL as _)
            as u32
    };
    if free < MIN_FREE_HEAP_FOR_TLS {
        if now.saturating_sub(*last_log) >= 30_000 {
            log::warn!("ws_uplink: 空きヒープ不足のため接続延期 ({free} bytes)");
            *last_log = now;
        }
        return false;
    }
    true
}

/// OTA 直後の image を無効にして前の image で再起動する (Refs #217)。
///
/// 戻す前に証跡 (内部RAM の空き・最低空き・止まっていた条件) を NVS に 1 行残し、
/// 戻った先の起動で `start` が `EVT OTA_ROLLED_BACK` として出す。
/// **戻ってきたら戻していない**: 既に確定済み (シリアル OTA の書き込み前に確定
/// した等) なら何もせず、戻し先が無ければ証跡を消して `EVT OTA_ROLLBACK_UNAVAILABLE`
fn rollback_ota(settings: &Settings, queue: &UplinkQueue, stall: &str) {
    if !crate::ota::running_app_pending() {
        return;
    }
    let (free_int, min_int) = unsafe {
        let caps = esp_idf_svc::sys::MALLOC_CAP_INTERNAL as _;
        (
            esp_idf_svc::sys::heap_caps_get_free_size(caps),
            esp_idf_svc::sys::heap_caps_get_minimum_free_size(caps),
        )
    };
    let note = format!("free_int={free_int} min_int={min_int} reason={stall}");
    let line = format!(
        "ws_uplink: OTA 後 {}分 WS に繋がらないため前の image に戻します ({note})",
        OTA_VERIFY_TIMEOUT_MS / 60_000
    );
    log::warn!("{line}");
    crate::crashlog::note(&line);
    settings.set_ota_rollback_note(&note);
    settings.set_ws_last_seq(queue.last_seq());
    std::thread::sleep(core::time::Duration::from_millis(300));
    let err = unsafe { esp_idf_svc::sys::esp_ota_mark_app_invalid_rollback_and_reboot() };
    log::warn!("ws_uplink: 前の image へ戻せません (err={err})");
    let _ = settings.take_ota_rollback_note();
    alc_hub_common::evtlog::emit("EVT OTA_ROLLBACK_UNAVAILABLE");
}

/// 窓の測定を送り、送れたものに送信済みの印を付ける。
///
/// `unsent_only` が true なら**まだこの接続で送っていないぶんだけ**送る
/// (ack 駆動の即時送信)。false なら窓の全件を送る (再送周期・接続直後)。
/// 戻り値 true = 送信に失敗したので接続を捨てて再接続すべき
fn flush_queue(conn: &mut Option<Conn>, queue: &mut UplinkQueue, unsent_only: bool) -> bool {
    // 先に seq だけ集め、フレームは 1 件ずつ組む (窓 20 件ぶんの文字列を
    // 同時にヒープへ置かない)
    let targets: Vec<u64> = if unsent_only {
        queue.entries_unsent().map(|e| e.seq).collect()
    } else {
        queue.entries().map(|e| e.seq).collect()
    };
    let Some(c) = conn.as_mut() else {
        return false;
    };
    for seq in targets {
        let frame = match queue.entries().find(|e| e.seq == seq).map(measurement_frame) {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => {
                log::error!("ws_uplink: フレーム組立失敗 seq={seq}: {e}");
                continue;
            }
            // 送る前に窓から外れた (ここへは来ない)
            None => continue,
        };
        if let Err(e) = c.client.send(FrameType::Text(false), frame.as_bytes()) {
            log::warn!("ws_uplink: 送信失敗 seq={seq}: {e:?}");
            return true;
        }
        queue.mark_sent(seq);
    }
    false
}

/// WS の接続を「切れた」状態にする。**client は drop しない。**
///
/// esp-idf-svc の `Drop for EspWebSocketClient` は
/// `esp_websocket_client_close(..).unwrap()` を呼ぶが、**既に切断済みの client
/// では ESP_FAIL が返って panic する** (`ws/client.rs:623`、実機で頻発)。
/// サーバ (cf-alc-recorder の Durable Object) は hibernation やアイドルで
/// TCP FIN を送ってくるので、これは異常系ではなく定常的に起きる — 実測で
/// 接続が 30 秒しか保たずに落ちた例がある。
///
/// esp-idf 側は切断を検知すると `Reconnect after 10000 ms` と**自動再接続を
/// 予定する**。こちらから client を捨てる必要はないので、フラグだけ倒して
/// 再接続イベント (`WsEvent::Connected`) を待つ。
/// 接続を切断状態へ遷移させる。**既に切断済みなら何もしない** —
/// 呼び出し側 (WsEvent::Disconnected) はこの遷移が実際に起きたときだけ
/// `EVT WS_DISCONNECTED` を積む (毎回積むと 10 秒ごとの再接続失敗が
/// `.noinit` リング 4 KB を約 95 秒で一周させ、切り分けに要る EVT を
/// 押し出す。Refs #217)
fn mark_disconnected(conn: &mut Option<Conn>, reason: &str, queue: &UplinkQueue) -> bool {
    let Some(c) = conn.as_mut() else {
        return false;
    };
    if !c.connected {
        return false;
    }
    let held = match c.connected_at {
        Some(at) => format!("{}s", now_ms().saturating_sub(at) / 1000),
        None => "-".to_string(),
    };
    let line = format!(
        "ws_uplink: 接続断 ({reason}) held={held} queue={} last_seq={} — 自動再接続を待つ",
        queue.len(),
        queue.last_seq()
    );
    log::warn!("{line}");
    crate::crashlog::note(&line);
    c.connected = false;
    c.connected_at = None;
    c.disconnected_at = Some(now_ms());
    true
}

/// 測定をキューへ積み NVS へ永続化する。記録時の稼働時間と boot_id も持たせ、
/// NTP 未同期で記録した時刻を送信時に補正できるようにする (fix_unsynced_times)
fn enqueue(queue: &mut UplinkQueue, settings: &Settings, rec: &UplinkRecord, boot_id: u32) {
    let result = queue.push_record(
        rec.kind,
        rec.recorded_at_ms,
        &rec.payload,
        rec.session_id.as_deref(),
        Some(rec.at_ms),
        Some(boot_id),
    );
    let dropped = match &result {
        Ok(pushed) => pushed.dropped.as_ref(),
        Err(failed) => failed.dropped.as_ref(),
    };
    if let Some(DroppedEntry { seq, kind }) = dropped {
        // 捨てられたのが打刻だと賃金計算のデータが欠けるので kind まで出す
        log::warn!("ws_uplink: 保存先が一杯で seq={seq} ({kind}) を破棄");
        alc_hub_common::evtlog::emit(&format!("EVT WS_DROPPED {seq} {kind}"));
    }
    match result {
        Ok(_) => persist(settings, queue),
        Err(failed) => log::error!("ws_uplink: 測定を保存できません: {}", failed.reason),
    }
}

/// 採番カウンタだけを永続化する。**未 ack エントリ本体は push/ack のたびに
/// 保存先 (punchq) が 1 件単位で書いている**ので、ここでの書き戻しは無い
fn persist(settings: &Settings, queue: &UplinkQueue) {
    settings.set_ws_last_seq(queue.last_seq());
}

fn publish_status(status: &SharedStatus, queue: &UplinkQueue, connected: bool) {
    if let Ok(mut st) = status.lock() {
        st.ws_connected = connected;
        st.ws_queue_len = queue.len();
        st.ws_last_seq = queue.last_seq();
    }
}

/// 下りフレームの処理 (ack 消し込み / command 中継)。
/// **戻り値 true = 窓に次の送信対象が載ったので即座に送ってよい** (Refs #142)
#[allow(clippy::too_many_arguments)]
fn handle_downlink(
    text: &str,
    queue: &mut UplinkQueue,
    settings: &Settings,
    conn: &mut Option<Conn>,
    print: &mut Option<PrintSession>,
    ui_tx: &Sender<UiCommand>,
    status: &SharedStatus,
    ev_tx: &mpsc::Sender<WsEvent>,
) -> bool {
    match parse_downlink(text) {
        Ok(Downlink::Ack { seq }) => {
            let acked = queue.ack(seq);
            if acked.removed {
                persist(settings, queue);
            }
            // 窓が保存先から埋まったぶんだけ、続けて送る対象がある
            return acked.refilled > 0;
        }
        Ok(Downlink::ServerError { seq, message }) => {
            // キューに残して次の再送周期で送り直す
            log::warn!("ws_uplink: サーバエラー seq={seq:?}: {message}");
        }
        Ok(Downlink::Command { id, payload }) => {
            println!("EVT WS_COMMAND {id} {payload}");
            // 遠隔 MEASURE 指示は点呼画面を開く。OTA 指示は firmware 更新を
            // 開始する (web からの遠隔更新経路、ota.rs 参照)。それ以外の解釈は
            // ホスト側
            match command_action(&payload).as_deref() {
                Some("measure") => {
                    let _ = ui_tx.send(UiCommand::Measure);
                    send_command_result(conn, &id, "{}");
                }
                Some("ota") => match command_ota_url(&payload) {
                    Some(url) => {
                        // OTA 進捗を command_result (同 id で上書き) として WS に
                        // 送り返す → web は GET /commands/:id/result で追える。
                        // 送信は本ループに集約するため ev_tx 経由 (WsEvent::Outbound)
                        let ev = ev_tx.clone();
                        let cid = id.clone();
                        let sink: crate::ota::ProgressSink =
                            std::sync::Arc::new(move |payload: String| {
                                if let Ok(frame) = command_result_frame(&cid, &payload) {
                                    let _ = ev.send(WsEvent::Outbound(frame));
                                }
                            });
                        crate::ota::spawn_update(url, status.clone(), Some(sink));
                    }
                    None => {
                        alc_hub_common::evtlog::emit(
                            "EVT OTA NG 下り command に有効な url がありません",
                        );
                        send_command_result(
                            conn,
                            &id,
                            r#"{"phase":"error","message":"invalid url"}"#,
                        );
                    }
                },
                // 印刷指示 (印刷ブリッジ #38): PDF URL を取得しプリンターへ
                // 9100 送信する。宛先未設定・URL 不正は command_result で返す
                Some("print") => match command_print_url(&payload) {
                    Some(url) => match settings.printer_addr() {
                        Some(addr) => {
                            crate::printer::spawn_print(url, addr, status.clone());
                            send_command_result(conn, &id, r#"{"phase":"started"}"#);
                        }
                        None => send_command_result(
                            conn,
                            &id,
                            r#"{"phase":"error","message":"printer addr not set"}"#,
                        ),
                    },
                    None => send_command_result(
                        conn,
                        &id,
                        r#"{"phase":"error","message":"invalid url"}"#,
                    ),
                },
                // WS push 印刷 (#38): PDF 本体を下り WS で受けて 9100 へ流す。
                // recorder / public URL / R2 不要。print_begin で printer へ接続、
                // print_data チャンクを書き足し、print_end で flush して閉じる。
                Some("print_begin") => match settings.printer_addr() {
                    Some(addr) => match PrintSession::open(&addr) {
                        Ok(sess) => {
                            *print = Some(sess);
                            send_command_result(conn, &id, r#"{"phase":"started"}"#);
                        }
                        Err(e) => {
                            log::warn!("ws_uplink: 印刷 9100 接続失敗: {e}");
                            *print = None;
                            send_command_result(
                                conn,
                                &id,
                                r#"{"phase":"error","message":"printer connect failed"}"#,
                            );
                        }
                    },
                    None => send_command_result(
                        conn,
                        &id,
                        r#"{"phase":"error","message":"printer addr not set"}"#,
                    ),
                },
                Some("print_data") => match (print.as_mut(), command_print_chunk(&payload)) {
                    (Some(sess), Some(chunk)) => match sess.write_chunk(&chunk.data) {
                        Ok(()) => {
                            let payload = format!(
                                r#"{{"phase":"progress","seq":{},"bytes":{}}}"#,
                                chunk.seq, sess.bytes,
                            );
                            send_command_result(conn, &id, &payload);
                        }
                        Err(e) => {
                            log::warn!("ws_uplink: 印刷 9100 書き込み失敗: {e}");
                            *print = None; // セッション破棄
                            send_command_result(
                                conn,
                                &id,
                                r#"{"phase":"error","message":"printer write failed"}"#,
                            );
                        }
                    },
                    (None, _) => send_command_result(
                        conn,
                        &id,
                        r#"{"phase":"error","message":"no active print session"}"#,
                    ),
                    (Some(_), None) => send_command_result(
                        conn,
                        &id,
                        r#"{"phase":"error","message":"invalid print_data"}"#,
                    ),
                },
                Some("print_end") => match print.take() {
                    Some(sess) => {
                        let bytes = sess.bytes;
                        match sess.finish() {
                            Ok(()) => {
                                let payload = format!(r#"{{"phase":"done","bytes":{bytes}}}"#);
                                send_command_result(conn, &id, &payload);
                            }
                            Err(e) => {
                                log::warn!("ws_uplink: 印刷 flush 失敗: {e}");
                                send_command_result(
                                    conn,
                                    &id,
                                    r#"{"phase":"error","message":"printer flush failed"}"#,
                                );
                            }
                        }
                    }
                    None => send_command_result(
                        conn,
                        &id,
                        r#"{"phase":"error","message":"no active print session"}"#,
                    ),
                },
                // Windows GW (alc-gw) ハブ URL の遠隔設定 (auth-worker
                // /device/setup から。シリアルの `GW URL` と同じ NVS 保存先)
                Some("gw_url") => match command_gw_url(&payload) {
                    Some(url) => match settings.set_gw_url(&url) {
                        Ok(()) => send_command_result(conn, &id, r#"{"ok":true}"#),
                        Err(e) => {
                            log::error!("ws_uplink: GW URL 保存失敗: {e:?}");
                            send_command_result(
                                conn,
                                &id,
                                r#"{"ok":false,"message":"save failed"}"#,
                            );
                        }
                    },
                    None => send_command_result(
                        conn,
                        &id,
                        r#"{"ok":false,"message":"invalid url"}"#,
                    ),
                },
                // GW 接続状態の照会 (gw_link.rs が HubStatus に反映した値)。
                // url は実際の接続先候補 = NVS 設定 > beacon 自動発見 の順
                Some("gw_status") => {
                    let (connected, discovered) = status
                        .lock()
                        .map(|st| (st.gw_connected, st.gw_discovered_url.clone()))
                        .unwrap_or((false, String::new()));
                    let url = settings
                        .gw_url()
                        .or_else(|| (!discovered.is_empty()).then_some(discovered));
                    let payload = match url {
                        Some(url) => format!(r#"{{"connected":{connected},"url":"{url}"}}"#),
                        None => format!(r#"{{"connected":{connected},"url":null}}"#),
                    };
                    send_command_result(conn, &id, &payload);
                }
                // バージョン照会: 現在の firmware version + 実行スロットを返す
                // (web の「更新必要か」判定用、config::firmware_version_full が
                // manifest.json の version と同形)
                Some("version") => {
                    let payload = format!(
                        r#"{{"version":"{}","slot":"{}"}}"#,
                        alc_hub_common::config::firmware_version_full(),
                        crate::ota::running_slot(),
                    );
                    send_command_result(conn, &id, &payload);
                }
                // 電源/バッテリー照会: UI ループが AXP2101 から読んで HubStatus に
                // キャッシュした値を返す (i2c は UI ループが所有するため、ここでは
                // 共有状態を読むだけ)。/device/setup から brownout / 充電の
                // 切り分けに使う (Refs #50, #52)
                Some("battery") => {
                    let payload = status
                        .lock()
                        .map(|st| {
                            format!(
                                r#"{{"read":{},"percent":{},"mv":{},"vbus":{},"charge":{}}}"#,
                                st.power_read,
                                st.battery_percent,
                                st.battery_mv,
                                st.vbus_present,
                                st.charge_state,
                            )
                        })
                        .unwrap_or_else(|_| r#"{"read":false}"#.to_string());
                    send_command_result(conn, &id, &payload);
                }
                // 直近ログの取得 (#195): crash_log / `LOG DUMP` と同じ `.noinit`
                // リングの末尾 (行境界) を command_result で返す。auth-worker の
                // MCP get_device_log が読む。上限は payload の max_bytes
                // (省略時 3000、1〜3800 にクランプ)。command_result は NVS
                // キューを通らず socket 直書きなので MAX_LINE_BYTES には掛からない。
                // USB ホスト (運行者 PWA) が居れば、上の `EVT WS_COMMAND` を合図に
                // PWA が返す `PWALOG` 行を最大 2 秒集めて `pwa_log` に足す (#215、
                // pwalog.rs)。**待つあいだ status の lock を持たない**
                Some("get_log") => {
                    let text = crate::crashlog::snapshot_text();
                    let (reset_history, usb_host) = status
                        .lock()
                        .map(|st| (st.reset_history, st.usb_host))
                        .unwrap_or((None, false));
                    let pwa = if usb_host {
                        crate::pwalog::collect(&id)
                    } else {
                        alc_hub_core::pwalog::PwaLog::NoHost
                    };
                    let payload = alc_hub_core::crashlog::log_payload(
                        &text,
                        command_log_max_bytes(&payload),
                        now_ms(),
                        reset_history,
                        &pwa,
                    );
                    send_command_result(conn, &id, &payload);
                }
                // M-Bus 5V の照会 (設定は無い、#202): USB ホストの有無と、
                // それに追随して hub-ui が実際に出しているか。`power_read` は
                // AXP2101 を一度でも読めたか — 起動後 10 秒は false のままなので、
                // UI は battery_present を「不明」と描き分けられる
                Some("bus5v_status") => {
                    let (usb_host, ext_5v_out, battery_present, power_read, bus_in) = status
                        .lock()
                        .map(|st| {
                            (
                                st.usb_host,
                                st.ext_5v_out,
                                st.battery_present,
                                st.power_read,
                                st.bus_in,
                            )
                        })
                        .unwrap_or((false, false, false, false, None));
                    let bus_in_json = match bus_in {
                        Some(true) => "true",
                        Some(false) => "false",
                        None => "null",
                    };
                    let payload = format!(
                        r#"{{"usb_host":{usb_host},"ext_5v_out":{ext_5v_out},"battery_present":{battery_present},"power_read":{power_read},"bus_in":{bus_in_json}}}"#
                    );
                    send_command_result(conn, &id, &payload);
                }
                // 遠隔再起動。OTA 中と点呼中は断る —
                // OTA は書き込み途中で切ると起動不能になり、点呼中の再起動は
                // 測定をやり直させる (点呼中の判定は src/main.rs の in_tenko と同じ)
                Some("reboot") => {
                    let busy = status
                        .lock()
                        .map_or(false, |st| st.ota_active || st.session_id.is_some());
                    if busy {
                        send_command_result(conn, &id, r#"{"ok":false,"message":"busy"}"#);
                    } else {
                        send_command_result(conn, &id, r#"{"ok":true}"#);
                        let line = "ws_uplink: 遠隔 reboot command により再起動します";
                        log::warn!("{line}");
                        crate::crashlog::note(line);
                        alc_hub_common::evtlog::emit("EVT WS_REBOOT_CMD");
                        settings.set_ws_last_seq(queue.last_seq());
                        std::thread::sleep(core::time::Duration::from_millis(300));
                        unsafe { esp_idf_svc::sys::esp_restart() };
                    }
                }
                // 未知の action も従来どおり空 result で ack する
                _ => send_command_result(conn, &id, "{}"),
            }
        }
        Ok(Downlink::Connected) | Ok(Downlink::Pong) => {}
        Err(e) => log::warn!("ws_uplink: 下りフレーム解析失敗: {e} ({text})"),
    }
    // ack 以外は窓を動かさないので、送信を早める理由が無い
    false
}

/// command への即時 command_result を送る (接続が生きていれば best-effort)。
fn send_command_result(conn: &mut Option<Conn>, id: &str, payload: &str) {
    let Some(c) = conn.as_mut() else { return };
    match command_result_frame(id, payload) {
        Ok(frame) => {
            if let Err(e) = c.client.send(FrameType::Text(false), frame.as_bytes()) {
                log::warn!("ws_uplink: command_result 送信失敗: {e:?}");
            }
        }
        Err(e) => log::error!("ws_uplink: command_result 組立失敗: {e}"),
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
        connected_at: None,
        disconnected_at: None,
    })
}
