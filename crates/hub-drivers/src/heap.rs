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
