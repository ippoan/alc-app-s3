//! キオスク PWA の診断ログを `get_log` の応答へ中継する口 (Refs ippoan/alc-app-s3#215)。
//!
//! USB の受信スレッド ([`crate::host_link`]) が `PWALOG ` 行を [`offer`] で横取りし、
//! 上限付きの channel で ws_uplink の `get_log` 処理へ渡す。[`collect`] は
//! `PWALOG END <id>` か時間切れ ([`pure::WAIT_MS`]) まで待って集める。行の解釈・
//! id の突き合わせ・切り詰めは `alc_hub_core::pwalog` (純粋・テスト済み)。
//!
//! **信頼の境界**: `PWALOG` 行の認可は既存のシリアルコマンド
//! ([`crate::console::handle_common`]) と同じく「USB で物理的に繋がっている相手を
//! 信頼する」。中身は `get_log` の応答 (`pwa_log`) にだけ入り、リング・crash_log
//! には入らない。

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use alc_hub_core::pwalog::{self as pure, Collector, PwaLog};

/// PWA の返事 (40 行 + `END`) に余裕を足した行数。溢れた行は捨てる
/// (受信スレッドを止めない)。1 行は `console::MAX_LINE` 以下
const CHANNEL_CAP: usize = 48;

struct Chan {
    tx: SyncSender<String>,
    rx: Mutex<Receiver<String>>,
}

static CHAN: OnceLock<Chan> = OnceLock::new();

fn chan() -> &'static Chan {
    CHAN.get_or_init(|| {
        let (tx, rx) = sync_channel(CHANNEL_CAP);
        Chan {
            tx,
            rx: Mutex::new(rx),
        }
    })
}

/// 受信した `PWALOG ` 行を渡す (USB の受信スレッドから)。一杯なら捨てる。
pub fn offer(line: &str) {
    let _ = chan().tx.try_send(line.to_string());
}

/// command id `id` の `PWALOG` 行を `END` か時間切れまで集める。
/// **呼び出し側は status の lock を持たないこと** (最大 2 秒ブロックする)。
pub fn collect(id: &str) -> PwaLog {
    let rx = chan().rx.lock().unwrap_or_else(|e| e.into_inner());
    let mut collector = Collector::new(id);
    let deadline = Instant::now() + Duration::from_millis(pure::WAIT_MS);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            // 前の get_log の遅れた行もここで読み捨てる (id が違う)
            Ok(line) => {
                if collector.feed(&line) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    collector.finish()
}
