//! OTA ファームウェア更新 (Wi-Fi / 将来は LAN 経由、Refs #25 の運用面)。
//!
//! トリガは 2 経路 (どちらも同じ `spawn_update` に合流):
//! 1. `OTA <url>` ホストコマンド (USB シリアル、host_link.rs)
//! 2. WS 下り command `{"action":"ota","url":"https://..."}` (ws_uplink.rs)
//!    — cf-alc-recorder の `POST /tenants/:t/devices/:d/command` から push
//!    できるため、web からの遠隔更新はこの経路を叩くだけ。
//!
//! イメージは espflash save-image の **app 単体イメージ** (merged ではない)。
//! CI が GitHub Pages の `firmware/alc-hub-cores3-app.bin` に公開する。
//!
//! 安全装置:
//! - `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` + 起動完了時の
//!   `mark_running_slot_valid` (main.rs) — 新 FW が起動途中で死ぬと
//!   ブートローダが自動で旧スロットへ戻す
//! - ダウンロード/書き込み失敗時は update を破棄して現行 FW のまま続行
//!
//! # ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT OTA_START slot=<label> url=<url>` | 更新開始 (slot = 現在の実行スロット) |
//! | `EVT OTA_PROGRESS <received>/<total>` | 進捗 (64KB 毎。total は不明なら 0) |
//! | `EVT OTA OK <bytes>` | 書き込み完了 — 直後に再起動する |
//! | `EVT OTA NG <理由>` | 失敗 (現行 FW のまま続行) |

use anyhow::{bail, Context, Result};
use enumset::EnumSet;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::task::thread::{MallocCap, ThreadSpawnConfiguration};
use esp_idf_svc::http::client::{Configuration as HttpConfiguration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::ota::EspOta;
use esp_idf_svc::sys;

use alc_hub_common::status::{now_ms, SharedStatus};

/// ダウンロードのタイムアウト (チャンク毎)
const HTTP_TIMEOUT_S: u64 = 30;
/// 受信チャンク。8KB (>4KB) なので PSRAM に確保される
const CHUNK: usize = 8 * 1024;
/// app 単体イメージとして妥当な最小サイズ (これ未満は誤 URL とみなす)
const MIN_IMAGE_BYTES: usize = 256 * 1024;
/// 進捗イベントの間隔 [bytes]
const PROGRESS_STEP: usize = 64 * 1024;

/// OTA 進捗の送出先 (JSON payload 文字列を受け取る)。WS 経路では
/// command_result フレームに包んで送り返すために使う (ws_uplink.rs)。
/// シリアル (host_link) 経由の OTA では None。
/// `Arc` (Box ではなく): スレッド起動失敗時 (spawn_update 参照) に、
/// 起動スレッドへ move した後でも呼び出し元スコープ側から同じシンクで
/// エラー通知を送れるようにするため (clone して両方に配る)。
pub type ProgressSink = std::sync::Arc<dyn Fn(String) + Send + Sync>;

/// 現在実行中のパーティションラベル ("ota_0" 等)。
pub fn running_slot() -> String {
    unsafe {
        let part = sys::esp_ota_get_running_partition();
        if part.is_null() {
            return "?".into();
        }
        core::ffi::CStr::from_ptr((*part).label.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

/// OTA 更新を専用スレッドで開始する (TLS ハンドシェイク用にスタック大きめ)。
/// 成功時は戻らない (書き込み完了 → esp_restart)。失敗はイベント出力のみで
/// 現行 FW のまま続行する。`progress` は WS 経路での遠隔進捗表示用 (シリアル
/// 経路では None、進捗は EVT OTA_* のみ)。
pub fn spawn_update(url: String, status: SharedStatus, progress: Option<ProgressSink>) {
    // スレッド本体へ move するのは clone の方。起動失敗 (spawn Err) 時に
    // 呼び出し元スコープの `progress` でエラー通知を送るため元を残す
    // (以前は progress を直接 move していたため、スレッド起動自体が失敗すると
    // クロージャごと破棄され WS 側に何も通知されず Web が無限に「開始しました」
    // のままタイムアウトする抜け穴があった)
    let progress_for_thread = progress.clone();
    let status_for_thread = status.clone();

    // OTA スレッドの 20KB スタックを内蔵SRAMではなくPSRAMから確保する。
    // 実機でRAM使用率89%の状態でこのスレッド自体の起動が失敗する障害を確認した
    // (Refs #91) — CoreS3のPSRAM(8MB)はほぼ未使用で余裕があるため、ここを
    // 逃がすだけでOTA起動の成功率が上がる。stack_alloc_caps は「この設定を
    // 呼んだスレッドが次に spawn するスレッド」に適用される thread-local な
    // 予約 (esp_pthread_set_cfg) なので、spawn 直後に必ず既定へ戻す
    // (戻し忘れると呼び出し元スレッド — ws_uplink の run() ループ等 — が
    // その後 spawn する別処理 — printer.rs の spawn_print 等 — も
    // 意図せず PSRAM スタックになってしまう)
    if let Err(e) = (ThreadSpawnConfiguration {
        stack_size: 20 * 1024,
        stack_alloc_caps: EnumSet::only(MallocCap::Spiram),
        ..Default::default()
    }
    .set())
    {
        log::warn!("ota: PSRAM スタック設定に失敗 ({e:?})。内蔵SRAMのまま起動を試みる");
    }
    let spawned = std::thread::Builder::new()
        .name("ota".into())
        .stack_size(20 * 1024)
        .spawn(move || {
            let status = status_for_thread;
            let progress = progress_for_thread;
            println!("EVT OTA_START slot={} url={url}", running_slot());
            if let Ok(mut st) = status.lock() {
                st.push_event(now_ms(), "OTA 更新開始");
            }
            // OTA 中は UI ループが 10s 以上 feed できず task_wdt が誤リセットする
            // (更新が毎回中断する実害、Refs #55)。UI タスクの WDT 監視を download の
            // 間だけ止める。RAII ガードなので panic / 早期 return でも必ず戻る。
            let result = {
                let _wdt_pause = alc_hub_common::wdt::OtaWdtPause::new();
                download_and_write(&url, progress.as_ref())
            };
            match result {
                Ok(bytes) => {
                    println!("EVT OTA OK {bytes}");
                    if let Ok(mut st) = status.lock() {
                        st.push_event(now_ms(), "OTA 完了 — 再起動");
                    }
                    // 再起動でこの WS 接続は切れる。web は "ok" を最後に見てから
                    // デバイス再接続を待つ。フレーム送出 → flush の猶予を取る
                    if let Some(s) = progress.as_ref() {
                        s(format!(r#"{{"phase":"ok","bytes":{bytes}}}"#));
                    }
                    FreeRtos::delay_ms(1500);
                    unsafe { sys::esp_restart() };
                }
                Err(e) => {
                    println!("EVT OTA NG {e:#}");
                    if let Ok(mut st) = status.lock() {
                        st.push_event(now_ms(), "OTA 失敗");
                    }
                    if let Some(s) = progress.as_ref() {
                        // 詳細メッセージは JSON 文字列として安全化 (引用符・改行除去)
                        let msg = format!("{e:#}").replace(['"', '\n', '\r', '\\'], " ");
                        s(format!(r#"{{"phase":"error","message":"{msg}"}}"#));
                    }
                }
            }
        });
    // 呼び出し元スレッドの以降の spawn (printer.rs の spawn_print 等) に
    // PSRAM スタック設定が漏れないよう、成否に関わらず既定へ戻す
    if let Err(e) = ThreadSpawnConfiguration::default().set() {
        log::warn!("ota: スレッド設定を既定へ戻せませんでした: {e:?}");
    }
    if spawned.is_err() {
        println!("EVT OTA NG スレッド起動失敗 (メモリ不足)");
        if let Ok(mut st) = status.lock() {
            st.push_event(now_ms(), "OTA 失敗 (メモリ不足)");
        }
        // WS 経路 (progress Some) では、ここで通知しないと web 側は何の
        // 進捗も受け取れないまま pollOta の 5 分タイムアウトまで
        // 「更新を開始しました...」の表示で固まる (実際に発生した障害)
        if let Some(s) = progress.as_ref() {
            s(r#"{"phase":"error","message":"OTA 用スレッドの起動に失敗しました (メモリ不足)"}"#.to_string());
        }
    }
}

/// firmware を GET し、もう一方の OTA スロットへストリーミング書き込みする。
/// 完了時に boot パーティションを切り替えて書き込みバイト数を返す。
fn download_and_write(url: &str, progress: Option<&ProgressSink>) -> Result<usize> {
    let mut conn = EspHttpConnection::new(&HttpConfiguration {
        crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
        timeout: Some(core::time::Duration::from_secs(HTTP_TIMEOUT_S)),
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

    let mut ota = EspOta::new().context("OTA 初期化に失敗 (パーティション構成を確認)")?;
    let mut update = ota.initiate_update().context("OTA スロットの準備に失敗")?;

    // 8KB チャンク (PSRAM) でストリーミング。失敗時は update を drop = 破棄
    let mut buf = vec![0u8; CHUNK];
    let mut received = 0usize;
    let mut next_progress = PROGRESS_STEP;
    loop {
        let n = match conn.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                update.abort().ok();
                bail!("ダウンロード中断: {e}");
            }
        };
        if let Err(e) = update.write_all(&buf[..n]) {
            update.abort().ok();
            bail!("フラッシュ書き込み失敗: {e}");
        }
        received += n;
        if received >= next_progress {
            println!("EVT OTA_PROGRESS {received}/{total}");
            if let Some(s) = progress {
                s(format!(
                    r#"{{"phase":"download","received":{received},"total":{total}}}"#
                ));
            }
            next_progress += PROGRESS_STEP;
        }
    }

    if received < MIN_IMAGE_BYTES {
        update.abort().ok();
        bail!("イメージが小さすぎます ({received} bytes) — URL を確認してください");
    }
    update
        .complete()
        .context("OTA 確定に失敗 (イメージ検証 NG の可能性)")?;
    Ok(received)
}

/// 起動が正常に完了したことをブートローダへ確定する (rollback 解除)。
/// OTA 直後の初回起動でここまで到達できなければ、次のリセットで
/// ブートローダが旧スロットへ自動で戻す。
pub fn mark_boot_valid() {
    if let Ok(mut ota) = EspOta::new() {
        let _ = ota.mark_running_slot_valid();
    }
}
