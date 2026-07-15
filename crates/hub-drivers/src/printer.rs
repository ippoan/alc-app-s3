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
use std::time::Duration;

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
    // ネットワーク (LAN) が上がる前に lwip の socket API を叩くと
    // `tcpip_send_msg_wait_sem (Invalid mbox)` の assert でリブートする
    // (実機で確認 — シリアルポート open のリセット直後に PRINT が届いた場合)。
    // 接続確立前は開始せずエラー応答で止める。
    //
    // 診断ログ (EVT PRINT_DIAG): lan_link が false と読める原因の切り分け用。
    // WS が繋がっているのに lan_link=false なら status/lock の不整合、
    // lock=poisoned なら他スレッド panic の巻き添えを疑う (実機ログで判定)。
    let lan_up = match status.lock() {
        Ok(s) => {
            println!(
                "EVT PRINT_DIAG lan_link={} wifi={} ws={} ip={}",
                s.lan_link, s.wifi_connected, s.ws_connected, s.lan_ip,
            );
            s.lan_link
        }
        Err(_) => {
            println!("EVT PRINT_DIAG lock=poisoned");
            false
        }
    };
    if !lan_up {
        println!("EVT PRINT NG LAN 未接続 (ETH_CONNECTED を待ってください)");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("print".into())
        .stack_size(20 * 1024)
        .spawn(move || {
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
    // 先にプリンターへ接続する (不通なら PDF を引く前に失敗させる)
    let mut printer = TcpStream::connect(printer_addr)
        .with_context(|| format!("プリンター {printer_addr} に接続できません"))?;
    printer
        .set_write_timeout(Some(Duration::from_secs(IO_TIMEOUT_S)))
        .context("送信タイムアウト設定失敗")?;

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

    let mut chunk = vec![0u8; CHUNK];
    let mut next_progress = PROGRESS_STEP;
    let sent = alc_hub_core::printer::copy_stream(
        |buf| conn.read(buf).map_err(|e| e.to_string()),
        |bytes| printer.write_all(bytes).map_err(|e| e.to_string()),
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
