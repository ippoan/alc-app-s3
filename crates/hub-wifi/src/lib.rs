//! Wi-Fi STA 管理 (wifi) と Improv Wi-Fi Serial ハンドラ (improv)。
//!
//! improv は ESP Web Tools / Pages の Wi-Fi 設定フォームからの設定要求を
//! 処理し、wifi で接続して結果を返す。フレーム解析の純粋部分は
//! alc-hub-core::improv にある。

pub mod improv;
pub mod wifi;
