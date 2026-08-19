//! スレッド名を FreeRTOS 側にも載せるための薄い層。
//!
//! `std::thread::Builder::name()` は Rust 側の識別子にしかならず、FreeRTOS の
//! タスク名は全部 `pthread` になる。その結果 `HEAP DUMP` のタスク一覧が
//!
//! ```text
//! HEAPDUMP TASK pthread prio=5 stack_min_free=13984
//! HEAPDUMP TASK pthread prio=5 stack_min_free=1088
//! ```
//!
//! となり、「どのスレッドがスタックを余らせているか」「どれが余裕ゼロで
//! 危ないか」を特定できない。スタックは内部RAM から取られ、合計 130KB 超と
//! 内部RAM の最大消費なので、ここが見えないと削減の当てが付けられない
//! (ws_uplink の TLS ゲートが通らない件の調査で行き詰まった、Refs #74)。
//!
//! `esp_pthread_set_cfg` は「次に生成される pthread」に効く設定なので、
//! spawn の直前に名前だけ差し込む。stack_size は従来どおり
//! `std::thread::Builder::stack_size()` (pthread attr) が優先されるため、
//! ここでは触らない。

use core::ffi::CStr;

use enumset::EnumSet;
use esp_idf_svc::hal::task::thread::{MallocCap, ThreadSpawnConfiguration};

/// 次に生成されるスレッドの FreeRTOS タスク名を指定する。
/// 各 spawn 地点が自分の名前を必ず設定する前提なので、後始末は不要。
/// 失敗しても名前が既定に戻るだけで動作に影響しないため無視する。
pub fn name_next(name: &'static CStr) {
    let _ = ThreadSpawnConfiguration {
        name: Some(name),
        ..Default::default()
    }
    .set();
}

/// 名前を付けたうえで、スタックを **PSRAM** から確保する。
///
/// スタックは既定で内部RAM から取られ、常駐スレッド分だけで 100KB を超える。
/// 内部RAM は ws_uplink の TLS ハンドシェイク (60KB ゲート) と食い合うため、
/// 内部RAM を必要としないスレッドは PSRAM へ逃がす (PSRAM は 8MB 中 40KB しか
/// 使っていない)。`ota.rs` が先行して同じことをしている。
///
/// # 使ってはいけないスレッド
///
/// **flash / NVS へ書くスレッドには使えない。** 書き込み中はキャッシュが
/// 無効になり、PSRAM 上のスタックを踏んだ瞬間に落ちる。該当するのは
/// `ws_uplink` (NVS 送信キュー) / `recorder` (測定ログ) / `host_link`
/// (AUTH SET・ROTATE 等の設定保存) / `ble` (`delete_all_bonds` がボンドを
/// NVS から消す) / `auth_mint` (TLS のため内部RAM が要る)。
pub fn name_next_psram(name: &'static CStr, stack_size: usize) {
    let cfg = ThreadSpawnConfiguration {
        name: Some(name),
        stack_size,
        // タスクスタックは 8bit アクセス可であることが要る。Spiram 単独だと
        // 指定が無効となり、黙って内部RAM のまま確保される (実機で確認)
        stack_alloc_caps: MallocCap::Spiram | MallocCap::Cap8bit,
        ..Default::default()
    };
    if let Err(e) = cfg.set() {
        // PSRAM 不可でも内部RAM のまま起動はできるので継続する
        log::warn!("task: {name:?} の PSRAM スタック設定に失敗 ({e:?})");
        name_next(name);
    }
}
