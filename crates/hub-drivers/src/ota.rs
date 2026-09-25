//! OTA ファームウェア更新 (Wi-Fi / 将来は LAN 経由、Refs #25 の運用面)。
//!
//! トリガは 3 経路。1 と 2 は同じ `spawn_update` (HTTP で取りにいく) に合流し、
//! 3 はイメージそのものを USB で受ける。スロットへの書き込み ([`SlotWriter`]) と
//! 排他 (`OTA_BUSY`) は 3 経路で共用する:
//! 1. `OTA <url>` ホストコマンド (USB シリアル、host_link.rs)
//! 2. WS 下り command `{"action":"ota","url":"https://..."}` (ws_uplink.rs)
//!    — cf-alc-recorder の `POST /tenants/:t/devices/:d/command` から push
//!    できるため、web からの遠隔更新はこの経路を叩くだけ。
//! 3. `OTA SERIAL <size> <flavor>` (Refs #279、[`serial_begin`])。LAN も Wi-Fi も
//!    無い機 (timecard の `station`) 向けに、ブラウザが Pages から取ったイメージを
//!    Web Serial で流し込む。確定はホストの `OTA CONFIRM` ([`confirm_serial`])、
//!    来なければ [`spawn_serial_confirm_watch`] が 10 分で戻す
//!
//! イメージは espflash save-image の **app 単体イメージ** (merged ではない)。
//! CI が GitHub Pages の `firmware/alc-hub-cores3-app.bin` に公開する。
//! Wi-Fi 版 (`lan` 無し) は [`use_wifi_image`] により、送られてきた URL を
//! `alc-hub-cores3-wifi-app.bin` へ読み替えて Wi-Fi 経由で取りにいく。
//!
//! 安全装置:
//! - `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` + 初回の WS 接続での
//!   [`confirm_running_app_if_pending`] (ws_uplink.rs、Refs #217)。web
//!   インストーラの boot.bin は espflash 同梱の bootloader (rollback 無し) な
//!   ので、この設定は bootloader 自体には効かず、app 側の API (NEW を書く)
//!   にだけ効く — 確定と戻しは app (このモジュール) が行う。登録済みの機が
//!   IP を持ったまま 10 分 WS に繋がらなければ ws_uplink が旧スロットへ戻す
//!   (`EVT OTA_ROLLED_BACK` は戻った先の起動で出る)
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
//! | `EVT OTA_CONFIRMED slot=<label>` | OTA 直後の image を確定した (rollback 解除) |
//! | `EVT OTA_SERIAL_PENDING slot=<label> timeout_ms=<n>` | シリアル OTA で入れた image が `OTA CONFIRM` を待っている (起動時に 1 回) |
//! | `EVT OTA_ROLLED_BACK <証跡>` | 前の起動で OTA 直後の image を戻した ([`report_previous_rollback`]) |
//! | `EVT OTA_ROLLBACK_UNAVAILABLE` | 戻そうとしたが戻し先が無かった ([`rollback_ota`]) |
//!
//! シリアル OTA の応答行 (`OTA READY` / `OTA ACK` / `OTA OK` / `OTA ERR` /
//! `OTA CONFIRMED`) は docs/console-protocol.md の「シリアル OTA」を正本とする。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;
use esp_idf_svc::http::client::{Configuration as HttpConfiguration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::ota::{EspOta, EspOtaUpdate};
use esp_idf_svc::sys;

use alc_hub_common::settings::Settings;
use alc_hub_common::status::{now_ms, SharedStatus};
use alc_hub_core::uplink::OTA_VERIFY_TIMEOUT_MS;

/// ダウンロードのタイムアウト (チャンク毎)
const HTTP_TIMEOUT_S: u64 = 30;

/// OTA を始めるのに最低限必要な内部RAM の空き。
///
/// **60KB から下げた。** OTA が落ちていた原因はヒープではなく PSRAM スタックで、
/// TLS の確立自体は空き 62KB でも成功している (実測)。高い閾値のままだと、
/// 起動直後などメモリが落ち着く前に更新をかけたときに `内部RAM 不足` で
/// 弾いてしまう (実際 62,607 バイトで通っており 60KB は際どかった)。
/// ここは「明らかに異常なときだけ止める」水準に置く。
const MIN_FREE_HEAP_FOR_OTA: u32 = 40 * 1024;

/// BLE の scan 1 周期 (5 秒) が終わって NimBLE がヒープと電波を手放すまでの
/// 待ち。scan の途中では pause 判定に入らないため、状態を見るのではなく固定で置く。
const BLE_SETTLE_MS: u32 = 5_500;

/// 内部RAM の空き [bytes]。PSRAM を含む総量で見ると TLS のガードが素通りする
/// ため、ws_uplink と同じく内部RAM 専用に測る。
fn free_internal() -> u32 {
    unsafe { sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL as _) as u32 }
}

/// OTA 実行中フラグを立てる/降ろす。ws_uplink は true の間 WS を張らず、
/// BLE は scan を止める。
fn set_ota_active(status: &SharedStatus, active: bool) {
    if let Ok(mut st) = status.lock() {
        st.ota_active = active;
    }
}

/// BLE が退くのを待ってから空きを測る。戻り値は待った後の空き。
///
/// **WS は切らない。** 以前は「WS の TLS が内部RAM を食っている」と考えて
/// 畳んでいたが、それは誤診断だった: mbedTLS は `CONFIG_MBEDTLS_EXTERNAL_MEM_ALLOC`
/// で PSRAM 割当になっており、畳んでも内部RAM はほとんど戻らない (実測
/// 74KB → 83KB)。OTA が落ちていた本当の原因は PSRAM スタックでの flash 書き込みで、
/// TLS の確立自体は毎回成功していた。WS を維持すれば**進捗 (download phase) を
/// web へ送り続けられる**ので、切る理由がない。
fn wait_for_heap() -> u32 {
    // BLE の scan 1 周期ぶん待って NimBLE に電波とヒープを手放させる
    FreeRtos::delay_ms(BLE_SETTLE_MS);
    let free = free_internal();
    alc_hub_common::evtlog::emit(&format!("EVT OTA_HEAP free_int={free}"));
    free
}
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

/// OTA が走っているか (HTTP 版・シリアル版の共通、Refs #279)。
/// 取るのは [`BusyGuard::acquire`] だけで、放すのは guard の drop — 成功時は
/// 再起動するので放さない
static OTA_BUSY: AtomicBool = AtomicBool::new(false);

/// [`OTA_BUSY`] を握っている間だけ生きる guard。OTA スレッドへ move し、
/// スレッドの終わり (失敗・早期 return・panic) で必ず放す。スレッドの起動に
/// 失敗したときもクロージャごと drop されるので放される
struct BusyGuard;

impl BusyGuard {
    /// 取れなければ (別の OTA が走っている) None
    fn acquire() -> Option<Self> {
        OTA_BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        OTA_BUSY.store(false, Ordering::SeqCst);
    }
}

static WIFI_IMAGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// CoreS3 の Wi-Fi 版 (`lan` 無し) が起動時に呼ぶ。以後の OTA は送られてきた
/// LAN 版 / dev 版の URL を Wi-Fi 版のイメージへ読み替える (hub-core ota_image)
pub fn use_wifi_image() {
    WIFI_IMAGE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// この機が載っているネットワークの版 (`version` 照会の `net`)。
/// CoreS3 の Wi-Fi 版だけが `"wifi"`、それ以外 (CoreS3 LAN 版・W5500 の Atom 系) は `"lan"`
pub fn net_label() -> &'static str {
    if WIFI_IMAGE.load(std::sync::atomic::Ordering::Relaxed) {
        "wifi"
    } else {
        "lan"
    }
}

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
    // シリアル OTA (または先行の HTTP OTA) が走っている間は始めない (Refs #279)。
    // 同じスロットへ 2 本が書くと、どちらの image も壊れる
    let Some(busy) = BusyGuard::acquire() else {
        alc_hub_common::evtlog::emit("EVT OTA NG busy (別の OTA が実行中)");
        if let Some(s) = progress.as_ref() {
            s(r#"{"phase":"error","message":"別の OTA が実行中です"}"#.to_string());
        }
        return;
    };
    let url = if WIFI_IMAGE.load(std::sync::atomic::Ordering::Relaxed) {
        alc_hub_core::ota_image::wifi_image_url(&url)
    } else {
        url
    };
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
    // 名前もここで一緒に指定する。別途 task::name_next を呼ぶと設定ごと
    // 上書きされ、PSRAM スタックの指定が消えてしまう
    // **スタックは内蔵SRAM に置く。PSRAM にしてはいけない。**
    //
    // flash への書き込み (OTA の `update.write_all`) はキャッシュを凍結する。
    // 凍結中は外部 SPI RAM に触れないため、ESP-IDF は「スタックが PSRAM に
    // 載っているタスクから呼ばれたら」assert で止める:
    //
    //   assert failed: esp_cache_freeze_caches_disable_interrupts
    //   esp_cache_utils.c:96 (s_task_stack_is_sane_when_cache_frozen())
    //
    // 以前は `stack_alloc_caps: Spiram | Cap8bit` を指定していたため、TLS 確立の
    // 直後 (= 最初の flash 書き込み) で必ず落ち、**OTA が一度も完走しなかった**
    // (実機のシリアルで上記 assert を確認、crash_log には ESP の abort ダンプが
    // 残らないため長く原因不明だった)。内蔵SRAM 20KB を積む余裕はある —
    // OTA 直前の実測で空き 83KB (`EVT OTA_HEAP`)。
    if let Err(e) = (ThreadSpawnConfiguration {
        name: Some(c"ota"),
        stack_size: 20 * 1024,
        ..Default::default()
    }
    .set())
    {
        log::warn!("ota: スレッド設定に失敗 ({e:?})");
    }
    let spawned = std::thread::Builder::new()
        .name("ota".into())
        .stack_size(20 * 1024)
        .spawn(move || {
            let _busy = busy;
            let status = status_for_thread;
            let progress = progress_for_thread;
            println!("EVT OTA_START slot={} url={url}", running_slot());
            if let Ok(mut st) = status.lock() {
                st.push_event(now_ms(), "OTA 更新開始");
            }
            // web 側は「更新を開始しました...」から次の command_result まで
            // 何の手がかりも無かった (デバイスが本当にコマンドを受け取り処理を
            // 始めたのか、単にネットワーク上で消えたのかが区別できない)。
            // download 開始前にここで一度 "started" を送り、以後は download の
            // 進捗 (64KB毎) が続く前提を web 側が示せるようにする。
            // `message` に表示用テキストを直接載せる: auth-worker (web) 側は
            // download/ok/error 以外の phase を「message があればそのまま表示、
            // 無ければ phase 名を表示」という汎用フォールバックで扱う設計にして
            // あるため、今後 phase を増やしても web 側の追加対応は不要になる
            // (progress bar 表示など download 同等の特別な UI が要る場合のみ
            // web 側の対応が要る)
            if let Some(s) = progress.as_ref() {
                s(r#"{"phase":"started","message":"デバイスが更新処理を開始しました — 通信を整理しています..."}"#.to_string());
            }

            // --- 内部RAM を空ける (Refs #116) ---
            // OTA は HTTPS の TLS をもう 1 本張る。定常空きは約 72KB しかなく、
            // WS の TLS を張ったままだとハンドシェイクのピークで枯渇して落ちる
            // (実機で確認: `esp-x509-crt-bundle: Certificate validated` の直後に
            // panic し、OTA が一度も完走しなかった)。
            //
            if let Some(s) = progress.as_ref() {
                s(r#"{"phase":"started","message":"BLE を止めて準備しています..."}"#.to_string());
            }
            set_ota_active(&status, true);
            let free = wait_for_heap();
            if free < MIN_FREE_HEAP_FOR_OTA {
                // ここで落とさず明示的に失敗させる。以前はガードが無く、
                // TLS ハンドシェイクの途中で無言のまま panic していた
                set_ota_active(&status, false);
                alc_hub_common::evtlog::emit(&format!(
                    "EVT OTA NG 内部RAM 不足 ({free} bytes)"
                ));
                if let Ok(mut st) = status.lock() {
                    st.push_event(now_ms(), "OTA 失敗 (内部RAM 不足)");
                }
                if let Some(s) = progress.as_ref() {
                    s(format!(
                        r#"{{"phase":"error","message":"内部RAM 不足のため更新を中止しました ({free} bytes)"}}"#
                    ));
                }
                return;
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
                    alc_hub_common::evtlog::emit(&format!("EVT OTA OK {bytes}"));
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
                    // WS / BLE を元に戻す (成功時は再起動するので不要)
                    set_ota_active(&status, false);
                    alc_hub_common::evtlog::emit(&format!("EVT OTA NG {e:#}"));
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
        alc_hub_common::evtlog::emit("EVT OTA NG スレッド起動失敗 (メモリ不足)");
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
    // (スレッド起動失敗・URL 誤り・HTTP 失敗では確定しない位置)
    let mut slot = SlotWriter::begin(&mut ota, sys::OTA_SIZE_UNKNOWN)?;

    // 8KB チャンク (PSRAM) でストリーミング。失敗時は slot を drop = 破棄
    let mut buf = vec![0u8; CHUNK];
    let mut next_progress = PROGRESS_STEP;
    loop {
        let n = match conn.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => bail!("ダウンロード中断: {e}"),
        };
        slot.write(&buf[..n])?;
        let received = slot.written();
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
    slot.finish()
}

/// もう一方の OTA スロットへの書き込み。**HTTP 版とシリアル版の共用** (Refs #279) —
/// 取り出し元 (HTTP の `conn.read` / シリアルの受信チャンク) だけが呼び出し側に
/// 残る。途中で drop すると `EspOtaUpdate` の drop が update を破棄する
/// (スロットは切り替わらない)。
///
/// **flash へ書くので、呼ぶスレッドのスタックは内部RAM に置くこと**
/// ([`spawn_update`] のコメント)
struct SlotWriter<'a> {
    update: EspOtaUpdate<'a>,
    written: usize,
}

impl<'a> SlotWriter<'a> {
    /// 走っている image を確定してから、次のスロットを開く。
    ///
    /// 確定するのは、OTA 直後の未確定 (PENDING_VERIFY) のままだと esp_ota_begin が
    /// ESP_ERR_OTA_ROLLBACK_INVALID_STATE で失敗するため。WS 経路は接続時に
    /// 確定済みだが、シリアルの `OTA <url>` / `OTA SERIAL` は WS が繋がる前にも
    /// 来る (ホストと話せている = 今の image は動いている)。
    ///
    /// `size` は `esp_ota_begin` の image_size — `OTA_SIZE_UNKNOWN` はスロット全体を
    /// 先に消し (数秒)、`OTA_WITH_SEQUENTIAL_WRITES` は書くたびに消す
    fn begin(ota: &'a mut EspOta, size: u32) -> Result<Self> {
        confirm_running_app_if_pending();
        let update = ota
            .initiate_update_with_known_size(size as usize)
            .context("OTA スロットの準備に失敗")?;
        Ok(Self { update, written: 0 })
    }

    fn write(&mut self, buf: &[u8]) -> Result<()> {
        if let Err(e) = self.update.write_all(buf) {
            bail!("フラッシュ書き込み失敗: {e}");
        }
        self.written += buf.len();
        Ok(())
    }

    fn written(&self) -> usize {
        self.written
    }

    /// esp_image の検証をして boot パーティションを切り替える。書いたバイト数を返す
    fn finish(self) -> Result<usize> {
        let written = self.written;
        if written < MIN_IMAGE_BYTES {
            bail!("イメージが小さすぎます ({written} bytes) — URL を確認してください");
        }
        self.update
            .complete()
            .context("OTA 確定に失敗 (イメージ検証 NG の可能性)")?;
        Ok(written)
    }
}

/// シリアル OTA のチャンク長 (`OTA READY <n>` で伝える)。ホストは 1 チャンクごとに
/// `OTA ACK` を待つ (stop-and-wait)
const SERIAL_CHUNK: usize = 4096;

/// シリアル OTA で、これだけバイトが来なければ中止する (`OTA ERR timeout`)
const SERIAL_IDLE_TIMEOUT_MS: u64 = 10_000;

/// シリアル OTA の書き込みスレッドのスタック。TLS を張らないので HTTP 版
/// (20KB) より小さい。esp_image の検証 (`complete`) が SHA-256 を回す分を見込む。
/// **内部RAM に置く** ([`spawn_update`] のコメント)
const SERIAL_STACK: usize = 12 * 1024;

/// `OTA SERIAL` を受けたあと、reader スレッドが生バイトを流し込む口 (Refs #279)。
///
/// [`serial_begin`] が作って [`SERIAL_SINK`] に置き、reader
/// (`console::spawn_reader`) が行を 1 本捌くたびに [`take_serial_sink`] で
/// 引き取る。引き取った reader は `size` バイトを行に分けずにここへ渡し、
/// [`SERIAL_CHUNK`] ごとに書き込みスレッドへ送る
pub struct SerialSink {
    tx: SyncSender<Vec<u8>>,
    remaining: usize,
    buf: Vec<u8>,
    /// 書き込みスレッドが失敗・時間切れで終わった (reader は行モードへ戻る)
    aborted: Arc<AtomicBool>,
}

impl SerialSink {
    /// `size` バイトを受け切った
    pub fn received_all(&self) -> bool {
        self.remaining == 0
    }

    /// 書き込みスレッドが先に終わった (`OTA ERR …` を出し済み)
    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    /// `bytes` の先頭から受け取れるだけ受け取り、消費したバイト数を返す。
    /// 残りは次の行 (`OTA CONFIRM` 等) なので呼び出し側が行として扱う
    pub fn feed(&mut self, bytes: &[u8]) -> usize {
        let n = bytes.len().min(self.remaining);
        let mut used = 0;
        while used < n {
            let take = (SERIAL_CHUNK - self.buf.len()).min(n - used);
            self.buf.extend_from_slice(&bytes[used..used + take]);
            used += take;
            self.remaining -= take;
            if self.buf.len() == SERIAL_CHUNK || self.remaining == 0 {
                if self.tx.send(core::mem::take(&mut self.buf)).is_err() {
                    // 書き込みスレッドが居ない (終わった後)
                    self.aborted.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
        used
    }
}

/// [`serial_begin`] → reader の受け渡し場所。reader は 1 本なので 1 枠
static SERIAL_SINK: Mutex<Option<SerialSink>> = Mutex::new(None);

/// 直前に捌いた行が `OTA SERIAL` を受け入れていれば、その受け口を返す
pub fn take_serial_sink() -> Option<SerialSink> {
    SERIAL_SINK.lock().ok()?.take()
}

/// 次に書くスロットの長さ [bytes]。取れなければ 0 (= どの size も `OTA ERR size`)
fn next_slot_len() -> usize {
    let part = unsafe { sys::esp_ota_get_next_update_partition(core::ptr::null()) };
    if part.is_null() {
        return 0;
    }
    unsafe { (*part).size as usize }
}

/// `OTA SERIAL <size> <flavor>` を捌く (Refs #279)。**reader スレッドの上で呼ぶ** —
/// 受け入れたら `OTA READY` を返す前に受け口を置くので、ホストが READY を見て
/// 送り始めたバイトは必ず生のまま受ける。
///
/// 応答は `OTA READY <SERIAL_CHUNK>` か `OTA ERR flavor|size|busy|begin`。
/// flash への書き込みは専用スレッド (内部RAM スタック) が行い、1 チャンクごとに
/// `OTA ACK <累計>`、最後に `OTA OK` (再起動) か `OTA ERR write|timeout|verify` を出す
pub fn serial_begin(
    size: u32,
    flavor: &str,
    own_flavor: &'static str,
    status: &SharedStatus,
    settings: &Settings,
) {
    let size = size as usize;
    if flavor != own_flavor {
        println!("OTA ERR flavor");
        return;
    }
    if size < MIN_IMAGE_BYTES || size > next_slot_len() {
        println!("OTA ERR size");
        return;
    }
    let Some(busy) = BusyGuard::acquire() else {
        println!("OTA ERR busy");
        return;
    };
    let (tx, rx) = sync_channel::<Vec<u8>>(2);
    let (begin_tx, begin_rx) = sync_channel::<bool>(1);
    let aborted = Arc::new(AtomicBool::new(false));
    let aborted_for_thread = Arc::clone(&aborted);
    let status = status.clone();
    let settings = settings.clone();
    if let Err(e) = (ThreadSpawnConfiguration {
        name: Some(c"ota_serial"),
        stack_size: SERIAL_STACK,
        ..Default::default()
    }
    .set())
    {
        log::warn!("ota: スレッド設定に失敗 ({e:?})");
    }
    let spawned = std::thread::Builder::new()
        .name("ota_serial".into())
        .stack_size(SERIAL_STACK)
        .spawn(move || {
            serial_write(
                size,
                &rx,
                &begin_tx,
                &aborted_for_thread,
                &status,
                &settings,
                busy,
            )
        });
    if let Err(e) = ThreadSpawnConfiguration::default().set() {
        log::warn!("ota: スレッド設定を既定へ戻せませんでした: {e:?}");
    }
    if spawned.is_err() {
        // busy はクロージャごと drop 済み
        println!("OTA ERR begin");
        return;
    }
    // esp_ota_begin の結果を待つ。OTA_WITH_SEQUENTIAL_WRITES なので先に消さず、すぐ返る
    match begin_rx.recv_timeout(core::time::Duration::from_millis(SERIAL_IDLE_TIMEOUT_MS)) {
        Ok(true) => {
            if let Ok(mut slot) = SERIAL_SINK.lock() {
                *slot = Some(SerialSink {
                    tx,
                    remaining: size,
                    buf: Vec::new(),
                    aborted,
                });
            }
            println!("OTA READY {SERIAL_CHUNK}");
        }
        _ => println!("OTA ERR begin"),
    }
}

/// シリアル OTA の書き込みスレッド本体。終わり方ごとに応答行を出す
fn serial_write(
    size: usize,
    rx: &Receiver<Vec<u8>>,
    begin_tx: &SyncSender<bool>,
    aborted: &AtomicBool,
    status: &SharedStatus,
    settings: &Settings,
    _busy: BusyGuard,
) {
    // BLE の scan を止め、WS を張らせない (HTTP 版と同じ扱い)
    set_ota_active(status, true);
    let result = {
        let _wdt_pause = alc_hub_common::wdt::OtaWdtPause::new();
        serial_receive(size, rx, begin_tx, settings)
    };
    match result {
        Ok(bytes) => {
            alc_hub_common::hostout::line("OTA OK");
            if let Ok(mut st) = status.lock() {
                st.push_event(now_ms(), "シリアル OTA 完了 — 再起動");
            }
            log::info!("ota: シリアル OTA 完了 ({bytes} bytes) — 再起動");
            // 応答行が USB へ出切るのを待つ
            FreeRtos::delay_ms(500);
            unsafe { sys::esp_restart() };
        }
        Err(reason) => {
            // **応答より先に立てる** — ホストが ERR を見て次の行を送る前に、
            // reader を行モードへ戻しておく
            aborted.store(true, Ordering::SeqCst);
            set_ota_active(status, false);
            // begin の失敗は reader (serial_begin) が `OTA ERR begin` を出す
            if reason != "begin" {
                alc_hub_common::hostout::line(&format!("OTA ERR {reason}"));
            }
            if let Ok(mut st) = status.lock() {
                st.push_event(now_ms(), "シリアル OTA 失敗");
            }
        }
    }
}

/// スロットを開いて `size` バイトを書き、検証まで済ませる。失敗は `OTA ERR <語>` の語
fn serial_receive(
    size: usize,
    rx: &Receiver<Vec<u8>>,
    begin_tx: &SyncSender<bool>,
    settings: &Settings,
) -> core::result::Result<usize, &'static str> {
    let Ok(mut ota) = EspOta::new() else {
        let _ = begin_tx.send(false);
        return Err("begin");
    };
    let mut slot = match SlotWriter::begin(&mut ota, sys::OTA_WITH_SEQUENTIAL_WRITES) {
        Ok(slot) => slot,
        Err(e) => {
            log::warn!("ota: シリアル OTA の開始に失敗: {e:#}");
            let _ = begin_tx.send(false);
            return Err("begin");
        }
    };
    let _ = begin_tx.send(true);
    let idle = core::time::Duration::from_millis(SERIAL_IDLE_TIMEOUT_MS);
    while slot.written() < size {
        // 時間切れ・受け口の消滅のどちらも timeout (slot の drop で破棄)
        let Ok(chunk) = rx.recv_timeout(idle) else {
            log::warn!(
                "ota: シリアル OTA の受信が途切れた ({}/{size})",
                slot.written()
            );
            return Err("timeout");
        };
        if let Err(e) = slot.write(&chunk) {
            log::warn!("ota: {e:#}");
            return Err("write");
        }
        alc_hub_common::hostout::line(&format!("OTA ACK {}", slot.written()));
    }
    // 印は切り替える前に立てる — 切り替えた後に立て損ねると、確定されないまま
    // 戻らない image になる。立てられなくても続ける (`OTA CONFIRM` は効く)
    if let Err(e) = settings.set_ota_serial_pending(true) {
        log::warn!("ota: 確定待ちの印を保存できません: {e:?}");
    }
    match slot.finish() {
        Ok(bytes) => Ok(bytes),
        Err(e) => {
            log::warn!("ota: {e:#}");
            let _ = settings.set_ota_serial_pending(false);
            Err("verify")
        }
    }
}

/// `OTA CONFIRM` を捌く (Refs #279)。確定待ちなら確定して印を消す。
/// 確定待ちでなくても `OTA CONFIRMED` を返す (冪等)
pub fn confirm_serial(settings: &Settings) {
    confirm_running_app_if_pending();
    if settings.ota_serial_pending() {
        if let Err(e) = settings.set_ota_serial_pending(false) {
            log::warn!("ota: 確定待ちの印を消せません: {e:?}");
        }
    }
    println!("OTA CONFIRMED");
}

/// シリアル OTA の確定待ちを見張る (Refs #279)。起動時に 1 回呼ぶ。
///
/// 印があって、実行中の image が未確定なら、起動から [`OTA_VERIFY_TIMEOUT_MS`] 後に
/// まだ印が残っていれば (`OTA CONFIRM` が来なければ) 前の image へ戻す。
/// **印の無い pending (web インストーラ / espflash で入れた機) には何もしない。**
/// 印があるのに確定済み (ws_uplink が WS 接続で確定した等) なら、古い印を消すだけ
pub fn spawn_serial_confirm_watch(settings: Settings) {
    if !settings.ota_serial_pending() {
        return;
    }
    if !running_app_pending() {
        let _ = settings.set_ota_serial_pending(false);
        return;
    }
    alc_hub_common::evtlog::emit(&format!(
        "EVT OTA_SERIAL_PENDING slot={} timeout_ms={OTA_VERIFY_TIMEOUT_MS}",
        running_slot()
    ));
    // 戻すときに NVS と otadata を書くので内部RAM スタック (name_next は内部RAM)
    crate::task::name_next(c"ota_watch");
    let spawned = std::thread::Builder::new()
        .name("ota_watch".into())
        .stack_size(6 * 1024)
        .spawn(move || {
            let wait = OTA_VERIFY_TIMEOUT_MS.saturating_sub(now_ms());
            std::thread::sleep(core::time::Duration::from_millis(wait));
            if !settings.ota_serial_pending() || !running_app_pending() {
                return;
            }
            rollback_ota(
                &settings,
                "serial_unconfirmed",
                "OTA CONFIRM が来ない",
                || {},
            );
            // 戻ってきた (戻し先が無い) — 再起動ループを避けて確定する
            confirm_running_app_if_pending();
            let _ = settings.set_ota_serial_pending(false);
        });
    if let Err(e) = spawned {
        log::warn!("ota: 確定待ちの見張りを起動できません: {e:?}");
    }
}

/// 前の起動で OTA 直後の image を戻していたら、その証跡を出して消す (Refs #217)。
/// 戻った先がこのコードを持つ image のときだけ読まれる。起動時に呼ぶ
/// (ws_uplink::start と atoms3-timecard の main。2 回呼んでも 2 回目は何も出ない)
pub fn report_previous_rollback(settings: &Settings) {
    if let Some(note) = settings.take_ota_rollback_note() {
        alc_hub_common::evtlog::emit(&format!("EVT OTA_ROLLED_BACK {note}"));
    }
}

/// OTA 直後の image を無効にして前の image で再起動する (Refs #217)。
///
/// 戻す前に証跡 (内部RAM の空き・最低空き・止まっていた条件 `reason`) を NVS に
/// 1 行残し、戻った先の起動で [`report_previous_rollback`] が `EVT OTA_ROLLED_BACK`
/// として出す。`why` はログ用の説明、`before_reboot` は戻す直前に呼ぶ
/// (ws_uplink は送信の seq を保存する)。
/// **戻ってきたら戻していない**: 既に確定済み (シリアル OTA の書き込み前に確定
/// した等) なら何もせず、戻し先が無ければ証跡を消して `EVT OTA_ROLLBACK_UNAVAILABLE`
pub fn rollback_ota(settings: &Settings, reason: &str, why: &str, before_reboot: impl FnOnce()) {
    if !running_app_pending() {
        return;
    }
    let (free_int, min_int) = unsafe {
        let caps = sys::MALLOC_CAP_INTERNAL as _;
        (
            sys::heap_caps_get_free_size(caps),
            sys::heap_caps_get_minimum_free_size(caps),
        )
    };
    let note = format!("free_int={free_int} min_int={min_int} reason={reason}");
    let line = format!(
        "ota: OTA 後 {}分 {why}ため前の image に戻します ({note})",
        OTA_VERIFY_TIMEOUT_MS / 60_000
    );
    log::warn!("{line}");
    crate::crashlog::note(&line);
    settings.set_ota_rollback_note(&note);
    before_reboot();
    std::thread::sleep(core::time::Duration::from_millis(300));
    let err = unsafe { sys::esp_ota_mark_app_invalid_rollback_and_reboot() };
    log::warn!("ota: 前の image へ戻せません (err={err})");
    let _ = settings.take_ota_rollback_note();
    alc_hub_common::evtlog::emit("EVT OTA_ROLLBACK_UNAVAILABLE");
}

/// 実行中の app が OTA 直後の未確定状態 (`PENDING_VERIFY` または `NEW`) か。
/// web インストーラで入れた機は空の otadata から起動し、bootloader が VALID を
/// 書く (`bootloader_utility.c:491-506`)。`espflash flash` は otadata を触らない
/// ので、以前 OTA した機を USB で焼き直すと active entry が NEW のまま残り、
/// pending 扱いになりうる (登録済みで IP あり・WS 無しなら 10 分後に別の slot へ
/// 戻る。未登録なら ws_uplink.rs の分岐で即確定)。
///
/// NEW を含める理由: web インストーラの boot.bin (espflash 4.5.0 同梱、IDF
/// release/v5.5 既定設定) は rollback を持たず、OTA 書き込み後も otadata を
/// NEW → PENDING_VERIFY に書き換えない (`bootloader_utility.c` の遷移は
/// `#ifdef CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` の中)。NEW のままだと確定も
/// 戻しも一切走らず #217 の安全網が丸ごと機能しない。NEW を pending として
/// 扱っても安全なのは、確定 (`esp_ota_mark_app_valid_cancel_rollback`) が NEW
/// からでも VALID を書き (IDF v5.5.3 `esp_ota_ops.c:912-923`)、戻し
/// (`esp_ota_mark_app_invalid_rollback_and_reboot`) も状態を検査せず前の slot の
/// 検査が通れば自分の entry に INVALID を書いて restart するため
/// (`esp_ota_ops.c:924-936,848-900`)。
///
/// sys を直に呼ぶ: `EspOta::new()` は 1 プロセス 1 個で OTA スレッドと衝突し、
/// esp-idf-svc 0.52.1 の `SlotState::Unverified` は NEW と PENDING_VERIFY を
/// 区別しない
pub fn running_app_pending() -> bool {
    let mut state: sys::esp_ota_img_states_t = 0;
    let err = unsafe {
        sys::esp_ota_get_state_partition(sys::esp_ota_get_running_partition(), &mut state)
    };
    err == sys::ESP_OK
        && (state == sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY
            || state == sys::esp_ota_img_states_t_ESP_OTA_IMG_NEW)
}

/// OTA 直後の未確定状態 (PENDING_VERIFY または NEW) のときだけ、この image を
/// 確定して rollback を解除する (Refs #217)。ippoan/alc-gw-p4 の
/// `ota_link_confirm_running_app` は PENDING_VERIFY だけを見る — あちらは IDF で
/// build した bootloader (rollback 有効) を使うため NEW のまま残ることが無く、
/// こことは判定が分かれる。確定後は VALID になるので、2 回目以降の呼び出しは
/// 何もしない。
///
/// 呼ぶのは 5 か所:
/// - ws_uplink の 2 か所: 初回の WS 接続 / `ota_guard` の確定 (判定できない機・戻せなかった後)
/// - 次のスロットを開く前 ([`SlotWriter::begin`]。`OTA <url>` と `OTA SERIAL` の両方)
/// - `OTA CONFIRM` ([`confirm_serial`])
/// - シリアル OTA の確定待ちで戻せなかった後 ([`spawn_serial_confirm_watch`])
pub fn confirm_running_app_if_pending() {
    if !running_app_pending() {
        return;
    }
    let err = unsafe { sys::esp_ota_mark_app_valid_cancel_rollback() };
    if err == sys::ESP_OK {
        alc_hub_common::evtlog::emit(&format!("EVT OTA_CONFIRMED slot={}", running_slot()));
    } else {
        log::warn!("ota: image の確定に失敗 (err={err})");
    }
}
