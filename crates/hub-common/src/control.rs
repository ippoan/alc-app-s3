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

/// 「次のスキャン前に血圧計 1 台分のボンドを外す」要求フラグ (Refs ippoan/alc-app#401)。
/// ws_uplink (hub-drivers) の下り command `bp_unbond` がセットし、BLE ループ (hub-ble) が
/// 消費する。片方向 — 結果は `EVT BP_UNBOND` と `bp_status` で見る
pub type BpUnbondFlag = Arc<AtomicBool>;

pub fn new_bp_unbond_flag() -> BpUnbondFlag {
    Arc::new(AtomicBool::new(false))
}
