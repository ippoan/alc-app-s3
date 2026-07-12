//! NTP (SNTP) 時刻同期。
//!
//! Wi-Fi/LAN で外部ネットワークに繋がると pool.ntp.org と同期し、システム時刻
//! (gettimeofday / SystemTime) が実時刻になる。測定ログはこれを使って日本時間で
//! 記録される (recorder.rs)。同期前は稼働時間表示にフォールバックする。
//!
//! 返り値の EspSntp は drop すると同期が止まるため、呼び出し側で保持し続ける。

use anyhow::Result;
use esp_idf_svc::sntp::EspSntp;

/// SNTP クライアントを起動する。ネットワーク接続後に自動で同期する。
pub fn start() -> Result<EspSntp<'static>> {
    Ok(EspSntp::new_default()?)
}
