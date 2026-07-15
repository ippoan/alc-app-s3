//! PDF のプリンター 9100 (raw) 印刷 (ippoan/alc-app-s3#38)。
//!
//! `PRINT <url>` コマンド / WS 下り `{"action":"print","url":"..."}` から
//! 呼ばれ、PDF を HTTP GET しながらプリンターの 9100/tcp へチャンク単位で
//! ストリーミング書き込みする。全体を貯めないためメモリはチャンク分 (8KB)
//! しか使わない。チャンクコピーの核は alc_hub_core::printer::copy_stream
//! (純粋・テスト済み)。ota.rs の download_and_write と同じ構造。
//!
//! # ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT PRINT_START url=<url> printer=<addr>` | 印刷開始 |
//! | `EVT PRINT_PROGRESS <sent>/<total>` | 進捗 (64KB 毎。total 不明なら 0) |
//! | `EVT PRINT OK <bytes>` | 全バイト送信完了 |
//! | `EVT PRINT NG <理由>` | 失敗 |

use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use esp_idf_svc::http::client::{Configuration as HttpConfiguration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::sys;

use alc_hub_common::status::{now_ms, SharedStatus};

/// HTTP / TCP のタイムアウト (チャンク毎)
const IO_TIMEOUT_S: u64 = 30;
/// 転送チャンク。PSRAM 無しの AtomS3 でも問題ないサイズに抑える
const CHUNK: usize = 8 * 1024;
/// 進捗イベントの間隔 [bytes]
const PROGRESS_STEP: usize = 64 * 1024;

/// 印刷を専用スレッドで開始する (TLS ハンドシェイク用にスタック大きめ)。
/// 結果はイベント出力のみ。同時印刷の直列化は呼び出し側の責務 (現状は
/// コンソール/WS からの手動トリガーのみなので未対策)。
pub fn spawn_print(url: String, printer_addr: String, status: SharedStatus) {
    // LAN 待ちは印刷スレッド内で行う (console スレッドを最大 20 秒ブロックしない)。
    let spawned = std::thread::Builder::new()
        .name("print".into())
        .stack_size(20 * 1024)
        .spawn(move || {
            // ネットワーク (LAN) が上がる前に lwip の socket API を叩くと
            // `tcpip_send_msg_wait_sem (Invalid mbox)` の assert でリブートする。
            // 以前は即 NG で弾いていたが、ポート open のリセット直後や W5500 の
            // リンク negotiation 過渡期に PRINT が届くと lan_link=false で誤って
            // 失敗した (実機: 静かな再起動を挟むと boot 直後に PRINT が来る #59)。
            // lan_link が立つまで最大 20 秒待つ (auth_link と同じ待機を流用)。
            println!("EVT PRINT_WAIT_LAN");
            if !crate::auth_link::wait_for_network(&status, 20_000) {
                let diag = status
                    .lock()
                    .map(|s| {
                        format!(
                            "lan_link={} wifi={} ws={} ip={}",
                            s.lan_link, s.wifi_connected, s.ws_connected, s.lan_ip
                        )
                    })
                    .unwrap_or_else(|_| "lock=poisoned".into());
                println!("EVT PRINT NG LAN 未接続 (20秒待機後も未確立: {diag})");
                return;
            }
            println!("EVT PRINT_START url={url} printer={printer_addr}");
            if let Ok(mut st) = status.lock() {
                st.push_event(now_ms(), "印刷開始");
            }
            match fetch_and_send(&url, &printer_addr) {
                Ok(bytes) => {
                    println!("EVT PRINT OK {bytes}");
                    if let Ok(mut st) = status.lock() {
                        st.push_event(now_ms(), &format!("印刷送信完了 {}KB", bytes / 1024));
                    }
                }
                Err(e) => {
                    println!("EVT PRINT NG {e:#}");
                    if let Ok(mut st) = status.lock() {
                        st.push_event(now_ms(), "印刷失敗");
                    }
                }
            }
        });
    if spawned.is_err() {
        println!("EVT PRINT NG スレッド起動失敗 (メモリ不足)");
    }
}

/// PDF を GET しながらプリンターへストリーミング送信し、送信バイト数を返す
fn fetch_and_send(url: &str, printer_addr: &str) -> Result<usize> {
    // 先に HTTP GET を完了させ、書き込める状態にしてからプリンターへ接続する。
    // 逆順 (プリンター接続 → HTTP GET → 書き込み) だと、TLS handshake 分
    // (数秒〜十数秒) プリンターとの TCP 接続がデータ無しで放置され、RAW/
    // JetDirect ポート側が job-start を待たずに詰まる (実機で write_all が
    // EAGAIN タイムアウト。PC から接続直後に即書き込むテストでは同じ
    // プリンターに正常印字できた = 空白時間が原因と特定 #65)。
    let mut conn = EspHttpConnection::new(&HttpConfiguration {
        crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
        timeout: Some(Duration::from_secs(IO_TIMEOUT_S)),
        ..Default::default()
    })
    .context("HTTP 接続の初期化に失敗")?;
    conn.initiate_request(Method::Get, url, &[])
        .context("リクエスト送信に失敗")?;
    conn.initiate_response().context("応答受信に失敗")?;
    let http_status = conn.status();
    if http_status != 200 {
        bail!("HTTP {http_status} (200 以外)");
    }
    let total: usize = conn
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // レスポンスが取得できてから接続する (接続直後にすぐ書き込める状態にする)
    // #68 診断: connect 所要時間 / connect→初回 write の遅延 / write 毎の所要時間を
    // EVT PRINT_DIAG で出す。原因確定後に撤去する
    let t_connect = Instant::now();
    let mut printer = TcpStream::connect(printer_addr)
        .with_context(|| format!("プリンター {printer_addr} に接続できません"))?;
    let connect_ms = t_connect.elapsed().as_millis();
    printer
        .set_write_timeout(Some(Duration::from_secs(IO_TIMEOUT_S)))
        .context("送信タイムアウト設定失敗")?;
    // #68 診断: Nagle 無効化の効果検証 (未着手項目)
    let nodelay_ok = printer.set_nodelay(true).is_ok();
    println!(
        "EVT PRINT_DIAG connect_ms={connect_ms} nodelay={} local={}",
        u8::from(nodelay_ok),
        printer
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "?".into()),
    );

    let mut chunk = vec![0u8; CHUNK];
    let mut next_progress = PROGRESS_STEP;
    let mut last_io = Instant::now();
    let sent = alc_hub_core::printer::copy_stream(
        |buf| conn.read(buf).map_err(|e| e.to_string()),
        |bytes| {
            let wait_ms = last_io.elapsed().as_millis();
            let t_write = Instant::now();
            let result = printer.write_all(bytes);
            println!(
                "EVT PRINT_DIAG write bytes={} wait_ms={wait_ms} write_ms={} ok={}",
                bytes.len(),
                t_write.elapsed().as_millis(),
                u8::from(result.is_ok()),
            );
            last_io = Instant::now();
            result.map_err(|e| e.to_string())
        },
        &mut chunk,
        |t| {
            if t >= next_progress {
                println!("EVT PRINT_PROGRESS {t}/{total}");
                next_progress += PROGRESS_STEP;
            }
        },
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    printer.flush().context("プリンターへの送信失敗 (flush)")?;
    // 9100 raw はレスポンスを返さない。close (drop) で送信完了
    Ok(sent)
}
