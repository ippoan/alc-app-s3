//! ホスト I/O 層: USB CDC ホストリンク / RS232 (FC-1200) / LAN スタブ。
//!
//! BLE は alc-hub-ble、Wi-Fi/Improv は alc-hub-wifi、ボード初期化は
//! alc-hub-board、共有基盤 (状態/設定/UI コマンド) は alc-hub-common に分離
//! されている (依存を枝分かれさせて並列ビルドを可能にするため)。

pub mod auth_link;
pub mod console;
pub mod crashlog;
// W5500 SPI Ethernet (AtomS3 + Atomic PoE Base / CoreS3 + Base LAN PoE v1.2)。
// **CONFIG_ETH_SPI_ETHERNET_W5500 を有効にした sdkconfig でのみコンパイルされる。**
// 有効なのは cores3 (root の sdkconfig.defaults) / atoms3-print / atoms3-timecard の
// 3 つで、**W5500 を持たない機 (ベンチの atoms3-nfc 等) では下の `lan` ごと
// コンパイルされない** — Ethernet を積まない機に Ethernet を要求しないための cfg。
// 新しい機を足すときは、W5500 が無くてもこの crate がビルドできる状態を保つこと
#[cfg(esp_idf_eth_spi_ethernet_w5500)]
pub mod eth_w5500;
pub mod gw_link;
pub mod heap;
pub mod host_link;
// Base LAN PoE v1.2 (CoreS3)。中身は eth_w5500 の薄いラッパ (lan.rs は 47 行で
// 本体は `eth_w5500::start(...)` 1 行) なので、**上と同じ cfg で揃える**。
// 揃っていないと W5500 を無効にした sdkconfig で `unresolved import
// crate::eth_w5500` になり、NFC しか使わない機まで Ethernet を sdkconfig に
// 足す羽目になる (issue #146 で実際に踏んだ — atoms3-nfc が初の W5500 無し利用者)。
// 呼び出し元は src/main.rs の #[cfg(feature = "lan")] 1 か所だけで、CoreS3 は
// sdkconfig で W5500 有効なのでこの cfg は CoreS3 では no-op
#[cfg(esp_idf_eth_spi_ethernet_w5500)]
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
