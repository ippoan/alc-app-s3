//! LAN Module 13.2 (W5500, Ethernet + PoE) — 未実装スタブ。
//!
//! plan/cores3-hub-consolidation.md (issue #102 参照):
//! - CoreS3 単体デフォルトピン: CS=G1 / RST=G0 / INT=G10。公式 M5Module-LAN-13.2 の
//!   examples/LinkStatus/LinkStatus.ino の board_M5StackCoreS3 分岐で確認済み。
//! - 割当変更は M5-Bus 側の JC ジャンパ 3 組 (INT / RST / CS、差し替え式)。
//!   シルク番号はバスピン (無印 Core 基準) で、CoreS3 実 GPIO への翻訳は下記
//!   (スタック互換ツール K128+M131+M136 で確定):
//!     INT: G35→G10 (default) / G34→G14
//!     RST: G0 →G0  (default) / G13→G7
//!     CS : G5 →G1  (default) / G15→G13
//! - ★RS232M と同時スタック時: LAN の CS (default G5=CoreS3 G1) が RS232M の CS
//!   (CoreS3 G1) と衝突する。CS ジャンパを G15 (=CoreS3 G13) へ動かし、本ドライバ
//!   実装時は CS=13 を使う。INT=10 / RST=0 はデフォルトのまま変更不要。
//! - 給電は本モジュールの PoE (IEEE802.3at) から行う設計。
//!
//! TODO: W5500 ドライバ (esp-idf の eth ドライバ or `w5500` crate) で
//! リンク監視とクラウド (alc-app backend) 接続を実装し、
//! `HubStatus::lan_link` を更新する。RS232M と同時スタックなら CS=13 で初期化する。

use alc_hub_common::status::SharedStatus;

pub fn start(_status: SharedStatus) {
    log::warn!("lan: LAN Module 13.2 (W5500) は未実装 — lan_link は常に false");
}
