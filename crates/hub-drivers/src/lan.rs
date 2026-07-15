//! LAN Module 13.2 (W5500, Ethernet + PoE) — 未実装スタブ。
//!
//! plan/cores3-hub-consolidation.md (issue #102 参照):
//! - CoreS3 ピン: CS=G1 / RST=G0 / INT=G10。公式 M5Module-LAN-13.2 の
//!   examples/LinkStatus/LinkStatus.ino の board_M5StackCoreS3 分岐で確認済み。
//! - 割当変更は M5-Bus 側の JC ジャンパ (CSN/RSTN/INTN の 3 組、差し替え式)。
//!   公式 doc の切替値は無印 Core 基準で、CoreS3 の変更後 GPIO は公式サンプルに
//!   定義が無い (回路図/実機で要確認)。
//! - RS232M Module と G10 (INT) が競合し得るため、両モジュールスタック時は
//!   RS232M の DIP を G10 に振らない/LAN の INTN ジャンパで逃がす等の調整が必要
//!   (実機で要確認)。
//! - 給電は本モジュールの PoE (IEEE802.3at) から行う設計。
//!
//! TODO: W5500 ドライバ (esp-idf の eth ドライバ or `w5500` crate) で
//! リンク監視とクラウド (alc-app backend) 接続を実装し、
//! `HubStatus::lan_link` を更新する。

use alc_hub_common::status::SharedStatus;

pub fn start(_status: SharedStatus) {
    log::warn!("lan: LAN Module 13.2 (W5500) は未実装 — lan_link は常に false");
}
