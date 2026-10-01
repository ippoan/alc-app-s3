//! C 側の異常で落ちたときの要点を、次の起動まで残す (Refs ippoan/alc-app#403)。
//!
//! CPU 例外 (Guru Meditation)・`abort()`・assert・スタック溢れ・watchdog は、
//! ESP-IDF の panic handler が USB のコンソールへ直接出して再起動する。リング
//! (`alc_hub_drivers::crashlog`) には何も入らず、後から原因を辿る材料が機体に
//! 残らない。そこで panic handler の本体 `esp_panic_handler` をリンカの
//! `--wrap` で差し替え (build.rs)、入口で要点を内部 RAM の `.noinit`
//! (`crashlog::panic_record_ptr`) に書いてから、元の本体を呼ぶ。次の起動の
//! `crashlog::init` がそれを `EVT CRASH_INFO` の 1 行にする。
//!
//! **CoreS3 のバイナリにだけ在る** (`--wrap` はこの crate の bin にしか掛けない)。
//!
//! # この中で使ってよいもの
//!
//! 走るのは panic の最中 (割り込み禁止・もう一方のコアは停止)。**Rust の panic・
//! lock・確保に入りうるものを 1 つも使わない**: fmt・alloc・log・Mutex・
//! `println!`・境界検査つきの添字・スライスの範囲指定・`unwrap`・除算・桁溢れ
//! 検査つきの加算。使うのは生のポインタの `read_volatile` / `write_volatile`、
//! `wrapping_add`、固定の上限つきのループだけ。ここで Rust の panic が起きると
//! panic hook (fmt と lock を使う) に入り、元の本体が走らないまま watchdog の
//! リセットになる = USB への dump も `reset=panic` の印も失う。
//!
//! 不正なポインタを読んで CPU 例外になった場合は panic handler にもう一度入り、
//! 3 回目の入口で ESP-IDF が諦めて再起動する (`esp_system/panic.c` の
//! `PANIC_ENTRY_COUNT_MAX`)。**magic を最後に書く**ので、そのときは記録なしになる。
//!
//! # 写した構造体
//!
//! `panic_info_t` は esp-idf-sys の bindgen 出力に無いので、[`PanicInfo`] に
//! 自前で写す。**ESP-IDF v5.5.3 の
//! `components/esp_system/include/esp_private/panic_internal.h` と同じ並び**
//! (ESP-IDF の版を上げるときは突き合わせ直すこと)。例外フレームは bindgen の
//! `XtExcFrame` をそのまま使う。

use core::ffi::{c_char, c_int, c_void};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use alc_hub_core::crashlog::{
    PanicRecord, PANIC_DETAIL_CAP, PANIC_REASON_CAP, PANIC_RECORD_MAGIC,
};
use esp_idf_svc::sys::XtExcFrame;

/// `panic_info_t` の先頭から 8 番目の欄 (`frame`) まで。9 番目の
/// `bool pseudo_excause` は読まないので写さない (末尾なので並びに影響しない)。
///
/// 読まない欄 (`description` / `details` / `state`) も、後ろの欄の位置を合わせる
/// ために実物と同じ大きさで並べる。
#[repr(C)]
#[allow(dead_code)]
struct PanicInfo {
    /// `int core`
    core: c_int,
    /// `panic_exception_t exception` (enum = 4 バイト)
    exception: u32,
    /// `const char *reason`
    reason: *const c_char,
    /// `const char *description`
    description: *const c_char,
    /// `panic_info_dump_fn_t details` (関数ポインタ)
    details: *const c_void,
    /// `panic_info_dump_fn_t state` (関数ポインタ)
    state: *const c_void,
    /// `const void *addr`
    addr: *const c_void,
    /// `const void *frame` (Xtensa では `XtExcFrame`)
    frame: *const c_void,
}

extern "C" {
    /// 元の本体 (`--wrap` が付ける別名)
    fn __real_esp_panic_handler(info: *mut c_void);
    /// `bool g_panic_abort` (`esp_system/panic.c`)。C の bool を Rust の bool で
    /// 読まない (0 / 1 以外のビット列は Rust の bool として不正)
    static g_panic_abort: u8;
    /// `char *g_panic_abort_details` — abort 系の詳細 (assert の文言・スタック
    /// 溢れのメッセージ等)。無ければ null
    static g_panic_abort_details: *const c_char;
}

/// `esp_panic_handler` の差し替え。要点を書き残してから、**必ず**元の本体を呼ぶ。
///
/// # Safety
///
/// ESP-IDF の `panic_handler` (`esp_system/port/panic_handler.c`) からだけ呼ばれる。
#[no_mangle]
pub unsafe extern "C" fn __wrap_esp_panic_handler(info: *mut c_void) {
    if !info.is_null() {
        capture(info as *const PanicInfo);
    }
    __real_esp_panic_handler(info);
}

/// 要点を `.noinit` の記録へ書く。既に記録が在れば触らない (最初の 1 件を残す —
/// panic handler の中で起きた 2 つ目の例外で上書きしない)。
unsafe fn capture(info: *const PanicInfo) {
    let rec: *mut PanicRecord = alc_hub_drivers::crashlog::panic_record_ptr();
    if read_volatile(addr_of!((*rec).magic)) == PANIC_RECORD_MAGIC {
        return;
    }
    let abort = read_volatile(addr_of!(g_panic_abort)) != 0;
    write_volatile(
        addr_of_mut!((*rec).core),
        read_volatile(addr_of!((*info).core)) as u32,
    );
    write_volatile(
        addr_of_mut!((*rec).exception),
        read_volatile(addr_of!((*info).exception)),
    );
    write_volatile(addr_of_mut!((*rec).abort), abort as u32);

    let frame = read_volatile(addr_of!((*info).frame)) as *const XtExcFrame;
    let (exccause, pc, a0, a1, excvaddr) = if frame.is_null() {
        // フレームが無ければ、分かるのは落ちた命令の番地だけ
        (0, read_volatile(addr_of!((*info).addr)) as u32, 0, 0, 0)
    } else {
        (
            read_volatile(addr_of!((*frame).exccause)) as u32,
            read_volatile(addr_of!((*frame).pc)) as u32,
            read_volatile(addr_of!((*frame).a0)) as u32,
            read_volatile(addr_of!((*frame).a1)) as u32,
            read_volatile(addr_of!((*frame).excvaddr)) as u32,
        )
    };
    write_volatile(addr_of_mut!((*rec).exccause), exccause);
    write_volatile(addr_of_mut!((*rec).pc), pc);
    write_volatile(addr_of_mut!((*rec).a0), a0);
    write_volatile(addr_of_mut!((*rec).a1), a1);
    write_volatile(addr_of_mut!((*rec).excvaddr), excvaddr);

    // 文字列はポインタを保存せず、先頭だけを写す (ポインタの先は次の起動には無い)
    copy_text(
        read_volatile(addr_of!((*info).reason)),
        addr_of_mut!((*rec).reason) as *mut u8,
        PANIC_REASON_CAP,
    );
    let detail = if abort {
        read_volatile(addr_of!(g_panic_abort_details))
    } else {
        core::ptr::null()
    };
    copy_text(detail, addr_of_mut!((*rec).detail) as *mut u8, PANIC_DETAIL_CAP);

    // ここまで落ちずに来たときだけ、記録を有効にする
    write_volatile(addr_of_mut!((*rec).magic), PANIC_RECORD_MAGIC);
}

/// NUL 終端の文字列の先頭を、長さ `cap` の欄へ写す。残りは 0 で埋める
/// (`src` が null なら全部 0)。`cap` より長ければ切る (NUL は付かない)。
unsafe fn copy_text(src: *const c_char, dst: *mut u8, cap: usize) {
    let src = src as *const u8;
    let mut ended = src.is_null();
    let mut i = 0usize;
    while i < cap {
        let b = if ended {
            0
        } else {
            read_volatile(src.wrapping_add(i))
        };
        if b == 0 {
            ended = true;
        }
        write_volatile(dst.wrapping_add(i), b);
        i = i.wrapping_add(1);
    }
}
