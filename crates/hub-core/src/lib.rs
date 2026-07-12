//! alc-hub-cores3 の純粋ロジック。
//!
//! ESP-IDF (xtensa) に依存しないコードをここへ分離し、ホスト上で単体テスト・
//! カバレッジ計測を可能にする (ippoan/rust-alc-api の alc-core と同じ役割)。
//! 本クレートのファイルは `coverage_100.toml` に登録され、PR CI で
//! ラインカバレッジ 100% が強制される。

pub mod cfg;
pub mod clock;
pub mod coex;
pub mod device;
pub mod ieee11073;
pub mod improv;
pub mod layout;
pub mod pairing;
pub mod protocol;
pub mod uplink;
pub mod vitals;
