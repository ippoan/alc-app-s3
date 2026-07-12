//! ホスト I/O 層: USB CDC ホストリンク / RS232 (FC-1200) / LAN スタブ。
//!
//! BLE は alc-hub-ble、Wi-Fi/Improv は alc-hub-wifi、ボード初期化は
//! alc-hub-board、共有基盤 (状態/設定/UI コマンド) は alc-hub-common に分離
//! されている (依存を枝分かれさせて並列ビルドを可能にするため)。

pub mod host_link;
pub mod lan;
pub mod rs232;
