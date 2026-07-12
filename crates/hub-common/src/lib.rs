//! alc-hub-cores3 の共有基盤。
//!
//! I/O 系クレート (hub-ble / hub-wifi / hub-drivers) と UI (hub-ui) の両方から
//! 参照される小さな土台。ここに置くのは「全員が使うもの」だけにし、
//! 依存の枝分かれ (= 並列ビルド) を壊さないこと。

pub mod config;
pub mod control;
pub mod measurement;
pub mod settings;
pub mod status;
pub mod ui_api;
