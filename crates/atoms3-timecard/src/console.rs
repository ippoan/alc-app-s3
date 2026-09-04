//! タイムカード端末のホストコンソール (USB Serial/JTAG、行指向)。
//!
//! 読み出しスレッドと機種非依存のコマンド (PING / HEAP / LOG / AUTH / WS) は
//! `alc_hub_drivers::console` が持つ。ここに書くのは**本機固有の分岐だけ**
//! (`STATUS` と `OTA`)。**印刷ブリッジや CoreS3 から丸写ししないこと** —
//! とくに `AUTH SET` (device credential を NVS へ書く口) を機種ごとに増やすと
//! provisioning の挙動が割れる。
//!
//! # 対応コマンド (ホスト → 端末)
//!
//! | コマンド | 説明 |
//! |---|---|
//! | `PING` | 疎通確認 (`PONG` 応答、共通実装) |
//! | `STATUS` | `STATUS LAN=1 IP=192.168.11.72 VER=0.1.0+abc1234 CLOCK=1 EPOCH=1788533000000` 応答 |
//! | `HEAP` / `HEAP DUMP` / `LOG DUMP` | ヒープ概況 / 詳細 / 直近ログ (共通実装) |
//! | `OTA <url>` | オンラインアップデート (`EVT OTA_*`、LAN 確立を待つ) |
//! | `AUTH SET/UNPAIR/STATUS/TOKEN/URL` | device credential 管理 (共通実装) |
//! | `WS URL <url>` / `WS STATUS` | cf-alc-recorder 常時接続の URL 上書き / 状態 (共通実装) |

use alc_hub_common::{
    config,
    settings::Settings,
    status::{epoch_ms, SharedStatus},
};
use alc_hub_core::protocol::{parse_line, HostCommand};
use alc_hub_core::uplink::MIN_SYNCED_MS;
use alc_hub_drivers::console;
use anyhow::Result;

pub fn start(status: SharedStatus, settings: Settings) -> Result<()> {
    console::spawn_reader(c"console", 8 * 1024, move |line| {
        handle_line(line, &status, &settings)
    })
}

fn handle_line(line: &str, status: &SharedStatus, settings: &Settings) {
    let command = match parse_line(line, 0) {
        Ok(Some(command)) => command,
        Ok(None) => return, // 空行
        Err(err_response) => {
            println!("{err_response}");
            return;
        }
    };

    // 機種に依らないコマンドは共通実装へ (hub-drivers/src/console.rs)。
    // 捌かれなかったものだけがここへ落ちてくる
    let Some(command) = console::handle_common(command, status, settings) else {
        return;
    };

    match command {
        HostCommand::Status => {
            let (lan, ip) = status
                .lock()
                .map(|s| (s.lan_link, s.lan_ip.clone()))
                .unwrap_or_default();
            // CLOCK は SNTP が効いているかの唯一の観測点。**打刻端末では時刻が命**
            // で、未同期のまま送ると 1970 起点の打刻が静かに入る (範囲内なので
            // DB 側で NULL にもならない)。判定は送信側と同じ MIN_SYNCED_MS を
            // 使う — ここだけ別の閾値にすると「CLOCK=1 なのに補正が走る」がありうる
            let epoch = epoch_ms();
            println!(
                "STATUS LAN={} IP={} VER={} CLOCK={} EPOCH={}",
                u8::from(lan),
                if ip.is_empty() { "-" } else { &ip },
                config::firmware_version_full(),
                u8::from(epoch >= MIN_SYNCED_MS),
                epoch,
            );
        }
        // オンラインアップデート (進捗・結果は EVT OTA_*)
        HostCommand::Ota { url } => console::handle_ota_lan_guarded(url, status),
        // 本機で意味を持たないコマンド (画面遷移 / 印刷 / BLE / Wi-Fi / CFG 等)
        other => {
            log::debug!("console: unsupported command: {other:?}");
            println!("ERR UNSUPPORTED (timecard)");
        }
    }
}
