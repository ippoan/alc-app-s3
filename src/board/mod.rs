//! M5Stack CoreS3 ボード固有の初期化・ドライバ。
//!
//! ピン構成・レジスタ値は M5GFX / M5Unified の CoreS3 実装を一次情報として
//! 移植したもの (機械調査ベース・実機未検証)。

pub mod display;
pub mod power;
pub mod touch;
