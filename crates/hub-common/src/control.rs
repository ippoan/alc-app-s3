//! クレート間で共有する制御フラグ。
//!
//! I/O クレート (hub-ble / hub-wifi / hub-drivers) が互いに直接依存せずに
//! 連携するための、小さな共有プリミティブを置く。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// 「次のスキャン前に全ボンドを消して再ペアリングする」要求フラグ。
/// host_link (hub-drivers) がセットし、BLE ループ (hub-ble) が消費する。
pub type PairFlag = Arc<AtomicBool>;

pub fn new_pair_flag() -> PairFlag {
    Arc::new(AtomicBool::new(false))
}
