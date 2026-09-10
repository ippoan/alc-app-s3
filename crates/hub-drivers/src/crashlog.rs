//! panic 前ログの保持と復帰後の自動送信 (Refs ippoan/alc-app-s3#43)。
//!
//! 「画面が切れた」時に何が起きていたかを後追いするための仕組み:
//!
//! 1. **panic 前ログの保持** — noinit 領域のリングバッファ (CoreS3 は PSRAM の
//!    `.ext_ram_noinit` に 256 KB、AtomS3 系は DRAM の `.noinit` に 4 KB。
//!    [`RING_CAP`]、#217) に
//!    - C コンポーネント (Wi-Fi/BLE 等) の `esp_log` 出力
//!      (`esp_log_set_vprintf` の tee hook)。**Rust `log` マクロはここを
//!      通らない** — EspLogger は newlib stdout へ `fwrite` で直接書くため
//!      `esp_log_write` を経由せず、vprintf hook に乗らない
//!      (esp-idf-svc 0.52.1 `src/log.rs:354-376`)
//!    - Rust panic のメッセージ + 発生位置 (`std::panic::set_hook`。ESP の
//!      abort ダンプは vprintf hook を通らないため、ここが唯一の捕捉点)
//!    - `println!` 系の重要行 (vprintf hook を通らないため `note()` で明示追記。
//!      `EVT ` 行は `alc_hub_common::evtlog::emit` 経由 (init が登録する)、
//!      ほかに heap.rs の `EVT HEAP` (60 秒ごと) と起動の区切り行、#215)
//!    を蓄積する。`.noinit` はソフトリセット (panic / WDT / esp_restart / USB
//!    reset) で内容が保持され、電源断では失われる (magic + 帳簿検証で判定)。
//!    保持されていれば**どの reset 理由でも引き継ぐ** — 起動ごとに
//!    `--- BOOT reset=<name> (<code>) ---` を挟み、前の起動の行も読める (#215)。
//! 2. **復帰後の自動送信** — 起動時に `esp_reset_reason()` を確認し、
//!    クラッシュ由来ならリング内容 + reset reason + version/slot を
//!    kind="crash_log" として既存の WS 送信キュー (NVS 永続・ack 冪等) に
//!    積む。cf-alc-recorder → rust-alc-api `hub_measurements` に保存される
//!    (rust-alc-api 側 allowlist に "crash_log" の追加が必要)。
//!    brownout 等で RAM が保持されなかった場合も reset reason だけは送る。
//!
//! リング操作・sanitize・payload 組立の計算部分は alc-hub-core::crashlog
//! (純粋・テスト済み)。

use core::ffi::{c_char, c_int};
use core::mem::MaybeUninit;
use std::io::Write as _;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use alc_hub_common::{
    measurement::UplinkRecord,
    status::{now_ms, SharedStatus},
};
use alc_hub_core::crashlog as pure;
use esp_idf_svc::sys;

/// リング容量。置き場は sdkconfig の `CONFIG_SPIRAM_ALLOW_NOINIT_SEG_EXTERNAL_MEMORY`
/// で切り替える (Refs #217):
///
/// - 有効な機 (CoreS3) — PSRAM (`.ext_ram_noinit`) に 256 KB (ログ 1 行 ~100
///   バイトとして直近 ~2600 行相当)。WS 再接続の失敗が続いても数十分は残る
/// - 無効な機 (AtomS3 系。PSRAM 非搭載・`IGNORE_NOTFOUND` 併用) — 内部 DRAM
///   (`.noinit`) に 4 KB (直近 ~40 行相当)。静的 DRAM を常時消費するため控えめ
///
/// IDF のリンカスクリプトは `.ext_ram_noinit` をこの Kconfig が有効なときしか
/// 定義しない。`#[link_section]` の直書きは C の `EXT_RAM_NOINIT_ATTR` と違って
/// 無効時に内部 RAM へ黙って落ちてくれない (orphan section になる) ので、
/// 置き場ごと cfg で分ける
#[cfg(esp_idf_spiram_allow_noinit_seg_external_memory)]
const RING_CAP: usize = 256 * 1024;
#[cfg(not(esp_idf_spiram_allow_noinit_seg_external_memory))]
const RING_CAP: usize = 4096;
/// WS payload に載せるログの上限。送信キューは 1 件 1 キー (punchq、#142) に
/// なったが、**1 行 (= 1 件) は NVS 文字列の上限 4000 バイト未満**である必要が
/// あるため、payload はそれに収まる大きさに抑える。
const MAX_WS_LOG_BYTES: usize = 1024;
/// "CRLG" — リングが前回稼働から保持されているかの判定 magic。
const MAGIC: u32 = 0x43524c47;
/// vprintf hook の 1 回あたりの整形バッファ。esp_log は概ね 1 呼び出し 1 行で、
/// 超過分は切り捨てる (hook は呼び出し元タスクのスタックで走るため控えめ)。
const LINE_BUF: usize = 256;

/// リング本体 (置き場は [`RING_CAP`] の doc)。ソフトリセットを跨いで内容が残る。
#[repr(C)]
struct Ring {
    magic: u32,
    /// 次の書き込み位置 (< RING_CAP)
    pos: u32,
    /// 有効バイト数 (<= RING_CAP)
    len: u32,
    data: [u8; RING_CAP],
}

#[cfg_attr(
    esp_idf_spiram_allow_noinit_seg_external_memory,
    link_section = ".ext_ram_noinit"
)]
#[cfg_attr(
    not(esp_idf_spiram_allow_noinit_seg_external_memory),
    link_section = ".noinit"
)]
static mut RING: MaybeUninit<Ring> = MaybeUninit::uninit();

/// リングへの排他。vprintf hook は複数タスクから同時に呼ばれ得る。
/// panic 中の再入で毒化しても書き込みは続行する (into_inner)。
static RING_LOCK: Mutex<()> = Mutex::new(());

/// 前回リセットがクラッシュ由来だった時の持ち越し情報。
pub struct CrashSnapshot {
    /// esp_reset_reason() の値
    pub reset_code: i32,
    /// panic 前のログ (sanitize 済み)。RAM が保持されなかった場合は空
    pub log: String,
}

fn ring_ptr() -> *mut Ring {
    // MaybeUninit<Ring> は Ring と同一レイアウト。u8/u32 は全ビットパターンが
    // 有効なため、電源断後のゴミも「読める」— 中身の信頼性は magic + 帳簿検証で
    // 判定する
    unsafe { core::ptr::addr_of_mut!(RING) as *mut Ring }
}

fn ring_write(bytes: &[u8]) {
    let _g = RING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let r = ring_ptr();
        // 自己修復: init() 前 (または電源投入直後のゴミ) に呼ばれても安全に
        // 書けるよう、magic/帳簿が不正ならここで初期化する。init() の配線漏れが
        // boot loop に化けた実害 (atoms3-print 2026-07-14) の再発防止
        if (*r).magic != MAGIC || !pure::ring_valid(RING_CAP, (*r).pos, (*r).len) {
            (*r).magic = MAGIC;
            (*r).pos = 0;
            (*r).len = 0;
        }
        let mut pos = (*r).pos;
        let mut len = (*r).len;
        pure::ring_append(&mut (*r).data, &mut pos, &mut len, bytes);
        (*r).pos = pos;
        (*r).len = len;
    }
}

/// 任意の 1 行をリングに残す (`println!` 系は vprintf hook を通らないため、
/// 残したい行は明示的にこれを呼ぶ。`EVT ` 行は `alc_hub_common::evtlog::emit`
/// が [`init`] で登録されたこれを呼ぶ)。
pub fn note(line: &str) {
    ring_write(line.as_bytes());
    ring_write(b"\n");
}

/// リングの現在内容を sanitize 済みの文字列で返す (古い順、`\n` 区切り)。
///
/// クラッシュ由来のリセットを待たずに読めるのが `report()` との違い。
/// 出口は 3 つ — `LOG DUMP` (シリアル、[`dump`]) と WS 下り command `get_log`
/// (command_result、ws_uplink.rs、#195) がここを共有する。
/// 帳簿が壊れている (init 前・電源断直後) なら空。
pub fn snapshot_text() -> String {
    unsafe {
        let r = ring_ptr();
        if (*r).magic == MAGIC && pure::ring_valid(RING_CAP, (*r).pos, (*r).len) {
            let raw = pure::ring_snapshot(&(*r).data, (*r).pos, (*r).len);
            pure::sanitize_log(&raw)
        } else {
            String::new()
        }
    }
}

/// リングの現在内容をホストへ吐き出す (`LOG DUMP`)。
///
/// 「LAN が切れたが再起動はしていない」ような、事象後に誰も繋いでいなかった
/// 障害の原因を後から取りに行くための口 (Refs #74)。
/// 応答は `LOGDUMP BEGIN` / `LOGDUMP <行>` … / `LOGDUMP END <行数>`。
pub fn dump() {
    let text = snapshot_text();
    println!("LOGDUMP BEGIN");
    let mut n = 0usize;
    for line in text.lines() {
        println!("LOGDUMP {line}");
        n += 1;
    }
    println!("LOGDUMP END {n}");
}

/// esp_log の vprintf hook。1 回の vsnprintf で整形し、リング追記と
/// コンソール出力 (stdout = USB Serial/JTAG) の両方へ流す。
/// va_list は一度しか消費できない (va_copy は Rust から使えない) ため、
/// 元の vprintf へは転送せず stdout へ書く — 既定実装も同じコンソールに
/// 書いているので出力先は変わらない。
unsafe extern "C" fn vprintf_tee(fmt: *const c_char, ap: sys::va_list) -> c_int {
    let mut buf = [0u8; LINE_BUF];
    let n = vsnprintf(buf.as_mut_ptr() as *mut c_char, buf.len(), fmt, ap);
    if n > 0 {
        // vsnprintf は切り捨て時も「書きたかった長さ」を返す
        let written = (n as usize).min(buf.len() - 1);
        ring_write(&buf[..written]);
        let mut out = std::io::stdout();
        let _ = out.write_all(&buf[..written]);
    }
    n
}

extern "C" {
    /// newlib の vsnprintf。esp-idf-sys の bindgen 出力に依存しないよう
    /// 自前宣言する (va_list 型は sys と共有)
    fn vsnprintf(s: *mut c_char, n: usize, format: *const c_char, ap: sys::va_list) -> c_int;
}

/// 起動直後 (他モジュールの初期化より前) に呼ぶ。
///
/// 前回リセットの解析 → リングの引き継ぎ (壊れていれば初期化) → 区切り行 →
/// `EVT ` 行の出口の登録 → `EVT BOOT` → hook 設置の順。戻り値は
/// `(reset_code, snapshot)`:
///
/// - `reset_code` — `esp_reset_reason()` の値 (`pure::reset_reason_name` /
///   `pure::is_usb_serial_reset` で分類する)。起動後いつ呼んでも同じ値なので、
///   ここで取ったものをそのまま持ち回る (警告デバイスの武装復元 #194 が使う)
/// - `snapshot` — クラッシュ由来のリセットだった場合の panic 前ログ。
///   WS キュー起動後に `report()` へ渡すこと
pub fn init() -> (i32, Option<CrashSnapshot>) {
    let reset_code = unsafe { sys::esp_reset_reason() } as i32;
    let mut snapshot = None;
    unsafe {
        let r = ring_ptr();
        let preserved =
            (*r).magic == MAGIC && pure::ring_valid(RING_CAP, (*r).pos, (*r).len);
        if pure::is_crash_reset(reset_code) {
            let log = if preserved {
                let raw = pure::ring_snapshot(&(*r).data, (*r).pos, (*r).len);
                pure::sanitize_log(&raw)
            } else {
                // 電源異常等で RAM が保持されなかった。reset reason だけ送る
                String::new()
            };
            snapshot = Some(CrashSnapshot { reset_code, log });
        }
        // 帳簿が有効なら**どの reset 理由でも**中身を残す (#215)。usb / sw の
        // 起動のたびに消していると、reset の前に何が起きたかを遠隔 (get_log) で
        // 読めない。電源断で壊れていれば今までどおり空から始める
        if !preserved {
            (*r).magic = MAGIC;
            (*r).pos = 0;
            (*r).len = 0;
        }
    }
    // 前の起動の行と今回の行の境目
    note(&pure::boot_separator(reset_code));
    // `EVT ` 行をリングにも残す口 (alc_hub_common::evtlog)。区切り行の後に
    // 登録し、次の EVT BOOT が区切り行の直後に並ぶようにする
    alc_hub_common::evtlog::set_sink(note);
    // 起動時の reset 理由を EVT で出す (setup ページの KNOWN フィルタに乗せる #59)。
    // usb/jtag/sw = シリアルポート open 等の無害なリセット (メール通知なし)、
    // panic/int_wdt/task_wdt/wdt/brownout/pwr_glitch/cpu_lockup = 異常
    // (is_crash_reset → crash_log 送信 + メール)。log::info! は "I (..)" 始まりで
    // setup ページに出ないため EVT で別途出す。
    alc_hub_common::evtlog::emit(&format!(
        "EVT BOOT reset={} ({reset_code})",
        pure::reset_reason_name(reset_code)
    ));

    // Rust panic のメッセージ + 位置をリングへ。hook から戻った後は既定どおり
    // abort → ESP panic handler → リセットに進む
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {info}\n");
        ring_write(msg.as_bytes());
        let mut out = std::io::stdout();
        let _ = out.write_all(msg.as_bytes());
        let _ = out.flush();
    }));

    unsafe {
        sys::esp_log_set_vprintf(Some(vprintf_tee));
    }

    (reset_code, snapshot)
}

/// 現在の epoch ms (NTP 未同期の起動直後は 1970 起点になる — サーバ側が
/// 受信時刻で補完する。recorder.rs と同じ割り切り)。
fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// クラッシュ snapshot を WS 送信キューへ積み、ホストと Log 画面へ通知する。
/// ws_uplink::start の後に呼ぶこと (channel が生きていれば NVS キューに
/// 永続化され、圏外・未ペアリングでも接続回復後に送られる)。
pub fn report(snap: &CrashSnapshot, ws_tx: &Sender<UplinkRecord>, status: &SharedStatus) {
    let reason = pure::reset_reason_name(snap.reset_code);
    alc_hub_common::evtlog::emit(&format!("EVT CRASH {reason} log_bytes={}", snap.log.len()));
    if let Ok(mut st) = status.lock() {
        st.push_event(now_ms(), &format!("crash 復帰 ({reason})"));
    }
    let payload = pure::crash_payload(
        snap.reset_code,
        &alc_hub_common::config::firmware_version_full(),
        &crate::ota::running_slot(),
        &snap.log,
        MAX_WS_LOG_BYTES,
    );
    let _ = ws_tx.send(UplinkRecord {
        kind: "crash_log",
        payload,
        recorded_at_ms: epoch_ms(),
        at_ms: now_ms(),
        // クラッシュ復帰レポートは点呼とは無関係 (そもそも前回起動の記録)
        session_id: None,
    });
}
