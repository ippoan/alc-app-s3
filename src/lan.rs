//! LAN Module 13.2 (W5500, Ethernet + PoE) — 未実装スタブ。
//!
//! plan/cores3-hub-consolidation.md (issue #102 参照):
//! - CoreS3 ピン: CS=G1 / RST=G0 / INT=G10 (LinkStatus.ino 既定)。
//!   ジャンパで INT=G34 / RST=G13 / CS=G15 に変更可。
//! - RS232M Module と G10 が競合し得るため、両モジュールスタック時は
//!   ジャンパ/DIP スイッチでの調整が必要 (実機で要確認)。
//! - 給電は本モジュールの PoE (IEEE802.3at) から行う設計。
//!
//! TODO: W5500 ドライバ (esp-idf の eth ドライバ or `w5500` crate) で
//! リンク監視とクラウド (alc-app backend) 接続を実装し、
//! `HubStatus::lan_link` を更新する。

use crate::status::SharedStatus;

pub fn start(_status: SharedStatus) {
    log::warn!("lan: LAN Module 13.2 (W5500) は未実装 — lan_link は常に false");
}
