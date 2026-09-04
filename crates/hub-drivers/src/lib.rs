//! ホスト I/O 層: USB CDC ホストリンク / RS232 (FC-1200) / LAN スタブ。
//!
//! BLE は alc-hub-ble、Wi-Fi/Improv は alc-hub-wifi、ボード初期化は
//! alc-hub-board、共有基盤 (状態/設定/UI コマンド) は alc-hub-common に分離
//! されている (依存を枝分かれさせて並列ビルドを可能にするため)。

pub mod auth_link;
pub mod console;
pub mod crashlog;
// W5500 SPI Ethernet (AtomS3 + Atomic PoE Base)。CoreS3 の sdkconfig では
// CONFIG_ETH_SPI_ETHERNET_W5500 を有効にしていないためコンパイルされない
#[cfg(esp_idf_eth_spi_ethernet_w5500)]
pub mod eth_w5500;
pub mod gw_link;
pub mod heap;
pub mod host_link;
pub mod lan;
// NFC 読み取り (Unit NFC / ST25R3916)。components/nfc_shim の extern "C" を呼ぶため
// その component を取り込む crate だけが有効化できる
#[cfg(feature = "nfc")]
pub mod nfc;
pub mod ntp;
pub mod ota;
pub mod printer;
pub mod recorder;
pub mod rs232;
pub mod task;
// 音 (I2S + codec)。**NFC とは独立**の feature — 音だけ欲しい機 (点呼端末の
// 警告デバイス) がビルドできるようにするため分けてある (plan/standing-devices.md §2.3)
#[cfg(feature = "speaker")]
pub mod speaker;
pub mod ws_uplink;
