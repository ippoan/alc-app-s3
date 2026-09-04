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

/// ネットワークが確立してから SNTP を起動する (メインループから毎周期呼ぶ)。
///
/// # なぜ「起動直後に呼ぶ」ではいけないのか
///
/// `start()` を lwip の tcpip スレッドが立つ前に呼ぶと
/// `assert failed: tcpip_callback /IDF/components/lwip/lwip/src/api/tcpip.c:318
/// (Invalid mbox)` で panic し、**起動ループになる**
/// (2026-09-04 に AtomS3 Lite + Atomic PoE Base の実機で踏んだ)。
///
/// CoreS3 が `lan::start` の直後に呼んでも通るのは、**Wi-Fi の初期化が先に
/// esp_netif を立てている**から。Wi-Fi を持たない Atom 系では
/// `eth_w5500::start` が W5500 の初期化をスレッドへ逃がして即戻るので、
/// main の続きで呼ぶと必ず早すぎる。「CoreS3 で動いているから」で並びを
/// 写さないこと。
///
/// `has_ip` が立つまで何もせず、立ったら 1 回だけ起動して `slot` に保持する
/// (EspSntp は drop すると同期が止まるため、呼び出し側が持ち続けること)。
/// 起動に失敗しても panic させない — 次の周期で再試行する。
pub fn start_when_online(slot: &mut Option<EspSntp<'static>>, has_ip: bool) {
    if slot.is_some() || !has_ip {
        return;
    }
    match start() {
        Ok(sntp) => {
            log::info!("sntp: 起動 (ネットワーク確立後)");
            *slot = Some(sntp);
        }
        Err(e) => log::warn!("sntp: 起動失敗 (次の周期で再試行): {e:#}"),
    }
}
