//! LAN Module 13.2 (W5500, Ethernet + PoE) — CoreS3 スタック向け実装 (Refs #74)。
//!
//! W5500 の実体は eth_w5500.rs (AtomS3 + Atomic PoE Base と共通)。本モジュールは
//! CoreS3 + LAN Module 13.2 のピン確定と配線前提のドキュメントを担う薄い層。
//!
//! ピン (plan/cores3-hub-consolidation.md、スタック互換ツール K128+M131+M136 で確定):
//! - SPI: M-Bus 共有 (SCK=G36 / MISO=G35 / MOSI=G37) — **LCD と同一バス**。
//!   G35 は LCD の DC と二役のため、バス共有の実際は hub-board/display.rs の
//!   SharedDcInterface を参照 (LCD 書き込み中は W5500 転送がブロックされる)
//! - CS: **G13** (RS232M 併用時。JC ジャンパを G5 → G15 へ差し替え)。
//!   LAN 単体運用 (ジャンパ default G5) なら G1
//! - RST: G0 (JC ジャンパ default)
//! - INT: G10 (JC ジャンパ default) — esp-idf の polling モードを使うため未使用
//!   (eth_w5500.rs 既存方式。INT 割り込み対応が必要になったら配線済みなので可能)
//! - 給電: モジュールの PoE (IEEE802.3at) または CoreS3 の M-Bus 5V
//!   (power.rs の BUS_EN/BOOST_EN)
//!
//! リンク監視・EVT ETH_CONNECTED/DISCONNECTED・`HubStatus::lan_link`/`lan_ip`
//! 更新・初期化失敗時の `EVT ETH NG` (稼働継続) はすべて eth_w5500.rs が行う。

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::AnyOutputPin;
use esp_idf_svc::hal::spi::SpiDriver;

use alc_hub_common::status::SharedStatus;

use crate::eth_w5500;

/// LAN Module 13.2 (W5500) を初期化しリンク監視を開始する。
/// `spi` は LCD と共有する M-Bus/SPI2 バス (main.rs が leak 済み)。
/// `cs` は RS232M 併用スタックの G13 (main.rs 参照)。
/// 初期化失敗は `EVT ETH NG` のイベント出力のみで稼働継続する
pub fn start(
    spi: &'static SpiDriver<'static>,
    cs: AnyOutputPin<'static>,
    rst: AnyOutputPin<'static>,
    sysloop: EspSystemEventLoop,
    status: SharedStatus,
) -> Result<()> {
    eth_w5500::start(spi, cs, Some(rst), sysloop, status)
}
