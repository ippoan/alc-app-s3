//! ホスト I/O 層: USB CDC ホストリンク / RS232 (FC-1200) / LAN スタブ。
//!
//! BLE は alc-hub-ble、Wi-Fi/Improv は alc-hub-wifi、ボード初期化は
//! alc-hub-board、共有基盤 (状態/設定/UI コマンド) は alc-hub-common に分離
//! されている (依存を枝分かれさせて並列ビルドを可能にするため)。

pub mod auth_link;
// W5500 SPI Ethernet (AtomS3 + Atomic PoE Base)。CoreS3 の sdkconfig では
// CONFIG_ETH_SPI_ETHERNET_W5500 を有効にしていないためコンパイルされない
#[cfg(esp_idf_eth_spi_ethernet_w5500)]
pub mod eth_w5500;
pub mod heap;
pub mod host_link;
pub mod lan;
pub mod ntp;
pub mod ota;
pub mod printer;
pub mod recorder;
pub mod rs232;
pub mod ws_uplink;
