//! ヒープ監視: 内部RAM/PSRAM の継続計測 + OOM 発生時点の捕捉 (Refs #27)。
//!
//! 内部RAM は Wi-Fi + BLE + UI 後の定常空きが ~60KB しかなく、TLS(WSS)
//! ハンドシェイク中は一桁 KB まで落ちる。`BLE_INIT: Malloc failed` を実機で
//! 踏んだが後追いでは「誰が何 KB 要求して落ちたか」が分からなかったため、
//! 発生の瞬間を捕まえる仕組みを常設する。
//!
//! 1. **OOM 捕捉 (最重要)**: `heap_caps_register_failed_alloc_callback` で
//!    malloc 失敗のまさにその瞬間に
//!    `EVT OOM req=<bytes> caps=<flags> free_int=<n> free_psram=<n> min_int=<n>`
//!    を出力する。callback 内は割り込み/任意タスク文脈のため **アロケーション
//!    禁止** — ヒープを使わない `esp_rom_printf` のみ使う。
//! 2. **low-water mark の継続計測**: `HEAP_LOG_INTERVAL_MS` 毎に
//!    `EVT HEAP free_int=<n> min_int=<n> free_psram=<n>` を出力し、`HubStatus`
//!    にも反映する (Log 画面のサマリーに `min<n>K` が出る)。
//!    `esp_get_free_heap_size` は PSRAM を heap に入れると混ざるため、
//!    `heap_caps_get_free_size(MALLOC_CAP_INTERNAL)` で内部RAM 専用に測る。
//! 3. **オンデマンド**: `HEAP` ホストコマンド (host_link.rs) が `stats()` を
//!    即時応答する。
//! 4. **再起動時ダンプ (補助)**: `esp_register_shutdown_handler` で restart
//!    経路でもヒープ状態を 1 行残す (panic/OOM 経路は 1. が主)。

use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys;

use alc_hub_common::status::{now_ms, SharedStatus};

/// 定期計測の間隔。TLS ハンドシェイク中の瞬間的な落ち込みは low-water mark
/// (min_int) が拾うため、ログ量とのバランスでこの粒度にする (issue #27: 5〜10s)。
const HEAP_LOG_INTERVAL_MS: u32 = 5_000;

/// ヒープ計測値 [bytes]。
#[derive(Debug, Clone, Copy)]
pub struct HeapStats {
    /// 内部RAM の現在空き
    pub free_int: usize,
    /// 内部RAM の起動以来の最低空き (low-water mark) — 今一番効く数字
    pub min_int: usize,
    /// PSRAM の現在空き (未搭載/無効なら 0)
    pub free_psram: usize,
    /// 内部RAM のヒープ総量 (使用率計算用)
    pub total_int: usize,
    /// PSRAM のヒープ総量 (未搭載/無効なら 0)
    pub total_psram: usize,
}

/// 現在のヒープ状態を読む (`HEAP` ホストコマンド・定期計測の共通部)。
pub fn stats() -> HeapStats {
    unsafe {
        HeapStats {
            free_int: sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL as _),
            min_int: sys::heap_caps_get_minimum_free_size(sys::MALLOC_CAP_INTERNAL as _),
            free_psram: sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM as _),
            total_int: sys::heap_caps_get_total_size(sys::MALLOC_CAP_INTERNAL as _),
            total_psram: sys::heap_caps_get_total_size(sys::MALLOC_CAP_SPIRAM as _),
        }
    }
}

/// ヒープ詳細ダンプ (`HEAP DUMP` ホストコマンド、Refs #27)。
///
/// 内部RAM/PSRAM のブロック概況と、全タスクのスタック余裕 (high-water mark =
/// 起動以来の最小残量) を `HEAPDUMP ` プレフィックスの行で出力する。
/// 「内部RAM を誰が使っているか」「どのタスクのスタックを削れるか」の
/// 定量判断に使う。`uxTaskGetSystemState` は CONFIG_FREERTOS_USE_TRACE_FACILITY
/// が必要 (sdkconfig.defaults で有効化)。
pub fn dump() {
    // ── ヒープブロック概況 ──
    for (label, caps) in [
        ("INT", sys::MALLOC_CAP_INTERNAL),
        ("PSRAM", sys::MALLOC_CAP_SPIRAM),
    ] {
        let mut info: sys::multi_heap_info_t = unsafe { core::mem::zeroed() };
        unsafe { sys::heap_caps_get_info(&mut info, caps as _) };
        println!(
            "HEAPDUMP {label} free={} alloc={} largest_free={} min_free={} blocks={}",
            info.total_free_bytes,
            info.total_allocated_bytes,
            info.largest_free_block,
            info.minimum_free_bytes,
            info.total_blocks,
        );
    }

    // ── タスク別スタック余裕 ──
    unsafe {
        let n = sys::uxTaskGetNumberOfTasks() as usize;
        // 呼び出しと列挙の間に生えるタスクに備えて少し多めに確保する
        let mut tasks: Vec<sys::TaskStatus_t> = vec![core::mem::zeroed(); n + 4];
        let filled =
            sys::uxTaskGetSystemState(tasks.as_mut_ptr(), tasks.len() as _, core::ptr::null_mut());
        for t in &tasks[..filled as usize] {
            let name = core::ffi::CStr::from_ptr(t.pcTaskName as *const core::ffi::c_char)
                .to_string_lossy();
            // usStackHighWaterMark は残量の最小値。ESP-IDF はスタック長を byte で
            // 扱うため byte 単位 (vanilla FreeRTOS の word 単位と違う)。
            // Rust スレッドは FreeRTOS 名がすべて "pthread" になる点に注意
            // (スタックサイズで判別: 20K=ws_uplink / 12K=host_link / 8K=既定)
            println!(
                "HEAPDUMP TASK {name} prio={} stack_min_free={}",
                t.uxCurrentPriority, t.usStackHighWaterMark,
            );
            // USB Serial/JTAG の TX バッファ (1KB) を溢れさせない排出待ち
            // (20 行前後を連続出力すると行落ちする — 実機で確認)
            FreeRtos::delay_ms(5);
        }
        println!("HEAPDUMP END tasks={filled}");
    }
}

/// malloc 失敗の瞬間に呼ばれる (ISR/任意タスク文脈)。ヒープを一切使わずに
/// `esp_rom_printf` で 1 行残す — これで「誰が何 KB 要求して落ちたか」が確定する。
unsafe extern "C" fn on_alloc_failed(
    size: usize,
    caps: u32,
    _function_name: *const core::ffi::c_char,
) {
    sys::esp_rom_printf(
        b"EVT OOM req=%u caps=0x%x free_int=%u free_psram=%u min_int=%u\n\0".as_ptr()
            as *const core::ffi::c_char,
        size as u32,
        caps,
        sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL as _) as u32,
        sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM as _) as u32,
        sys::heap_caps_get_minimum_free_size(sys::MALLOC_CAP_INTERNAL as _) as u32,
    );
}

/// `esp_restart` 経路で最後のヒープ状態を残す (補助。panic/OOM は callback が主)。
unsafe extern "C" fn on_shutdown() {
    sys::esp_rom_printf(
        b"EVT HEAP_SHUTDOWN free_int=%u min_int=%u free_psram=%u\n\0".as_ptr()
            as *const core::ffi::c_char,
        sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL as _) as u32,
        sys::heap_caps_get_minimum_free_size(sys::MALLOC_CAP_INTERNAL as _) as u32,
        sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM as _) as u32,
    );
}

/// OOM callback / shutdown handler を登録し、定期計測スレッドを起動する。
///
/// 他モジュールの重いアロケーション (Wi-Fi/BLE/TLS) より先に呼ぶこと —
/// 登録前に起きた OOM は捕まえられない。
pub fn start(status: SharedStatus) -> Result<()> {
    unsafe {
        // 失敗しても監視が無いだけで運転は継続できるため、ログに留める
        let err = sys::heap_caps_register_failed_alloc_callback(Some(on_alloc_failed));
        if err != sys::ESP_OK {
            log::warn!("heap: failed_alloc_callback 登録失敗 ({err})");
        }
        let err = sys::esp_register_shutdown_handler(Some(on_shutdown));
        if err != sys::ESP_OK {
            log::warn!("heap: shutdown_handler 登録失敗 ({err})");
        }
    }

    std::thread::Builder::new()
        .name("heap_mon".into())
        .stack_size(3 * 1024)
        .spawn(move || monitor_loop(status))?;
    Ok(())
}

fn monitor_loop(status: SharedStatus) -> ! {
    let mut last_min = usize::MAX;
    loop {
        let s = stats();
        // ホスト/observability 向け (EVT プレフィックスで行解釈される)
        println!(
            "EVT HEAP free_int={} min_int={} free_psram={}",
            s.free_int, s.min_int, s.free_psram
        );
        if let Ok(mut st) = status.lock() {
            st.heap_free_int = s.free_int;
            st.heap_min_int = s.min_int;
            st.heap_free_psram = s.free_psram;
            st.heap_total_int = s.total_int;
            st.heap_total_psram = s.total_psram;
            // low-water 更新はイベントログにも残す (現地で経緯が追える)。
            // 初回は基準値の記録として必ず 1 行入る。NFC 追加前後の比較は
            // この min の推移で定量判断する (issue #27 受け入れ条件)。
            if s.min_int < last_min {
                st.push_event(now_ms(), &format!("heap min {}KB", s.min_int / 1024));
            }
        }
        if s.min_int < last_min {
            last_min = s.min_int;
        }
        FreeRtos::delay_ms(HEAP_LOG_INTERVAL_MS);
    }
}
