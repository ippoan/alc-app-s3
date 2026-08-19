//! スレッド名付与ヘルパーの再エクスポート。実体は hub-common (hub-ble など
//! hub-drivers に依存しない crate からも使うため)。

pub use alc_hub_common::task::{name_next, name_next_psram};
