//! M5Stack CoreS3 ボード固有の初期化・ドライバ。
//!
//! ピン構成・レジスタ値は M5GFX / M5Unified の CoreS3 実装を一次情報として
//! 移植したもの (机上調査ベース・実機確認は README の TODO 参照)。
//! 他クレートに依存しない独立葉クレート (並列ビルド可能)。

pub mod display;
pub mod power;
pub mod touch;
