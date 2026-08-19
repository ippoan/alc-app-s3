//! Base LAN PoE v1.2 (W5500, Ethernet + PoE) — CoreS3 スタック向け実装 (Refs #74)。
//!
//! W5500 の実体は eth_w5500.rs (AtomS3 + Atomic PoE Base と共通)。本モジュールは
//! CoreS3 + Base LAN PoE v1.2 のピン確定と配線前提のドキュメントを担う薄い層。
//!
//! ピン (plan/cores3-hub-consolidation.md「次期構成」):
//! - SPI: M-Bus 共有 (SCK=G36 / MISO=G35 / MOSI=G37) — **LCD と同一バス**。
//!   G35 は LCD の DC と二役のため、バス共有の実際は hub-board/display.rs の
//!   SharedDcInterface を参照 (LCD 書き込み中は W5500 転送がブロックされる)
//! - CS: **G9**
//! - RST: **G7**
//! - INT: G14 — esp-idf の polling モードを使うため未使用
//!   (eth_w5500.rs 既存方式。INT 割り込み対応が必要になったら配線済みなので可能。
//!   G14 は内蔵マイクと共用なので、使うならマイクを諦めること)
//! - 給電: 本体の PoE または CoreS3 の M-Bus 5V (power.rs の BUS_EN/BOOST_EN)
//!
//! 旧 LAN Module 13.2 は CS ジャンパが G1/G13 の二択で、G13 が内蔵スピーカーの
//! I2S DOUT と固定で競合していた (`lan` feature が既定 off だった理由)。
//! Base LAN PoE v1.2 では CS が G9 に出るためこの制約は無い。ただし
//! **本体の DB9 (RS232/RS485: RX=G13 / TX=G1) を使うとスピーカーと NFC に
//! 再び衝突する**ので、FC-1200 は RS232M Module 側の DB9 から取ること。
//!
//! リンク監視・EVT ETH_CONNECTED/DISCONNECTED・`HubStatus::lan_link`/`lan_ip`
//! 更新・初期化失敗時の `EVT ETH NG` (稼働継続) はすべて eth_w5500.rs が行う。

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::AnyOutputPin;
use esp_idf_svc::hal::spi::SpiDriver;

use alc_hub_common::status::SharedStatus;

use crate::eth_w5500;

/// Base LAN PoE v1.2 (W5500) を初期化しリンク監視を開始する。
/// `spi` は LCD と共有する M-Bus/SPI2 バス (main.rs が leak 済み)。
/// `cs` は G9 / `rst` は G7 (main.rs 参照)。
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
