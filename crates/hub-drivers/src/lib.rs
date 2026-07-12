//! alc-hub-cores3 のデバイス I/O 層。
//!
//! 画面コード (バイナリ側 src/ui) より変更頻度が低いモジュールを集約し、
//! 画面遷移の変更でこのクレートが再コンパイルされないようにする。
//! ESP-IDF (xtensa) 依存のため、純粋ロジックは alc-hub-core 側に置くこと。

pub mod ble;
pub mod board;
pub mod config;
pub mod host_link;
pub mod improv;
pub mod lan;
pub mod rs232;
pub mod settings;
pub mod status;
pub mod ui_api;
pub mod wifi;
