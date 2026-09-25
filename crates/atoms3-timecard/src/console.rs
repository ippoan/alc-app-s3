//! タイムカード端末のホストコンソール (USB Serial/JTAG、行指向)。
//!
//! 読み出しスレッドと機種非依存のコマンド (PING / DEVICE / HEAP / LOG / AUTH /
//! WS) は `alc_hub_drivers::console` が持つ (正本は
//! [`docs/console-protocol.md`](../../../docs/console-protocol.md))。
//! ここに書くのは**本機固有の分岐だけ** (`STATUS` と `OTA`、`vein` feature の `VEIN`)。
//! **印刷ブリッジや CoreS3 から丸写ししないこと** —
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
//! | `PAIR` | 血圧計の再ペアリング (ボンド消去 + 120 秒のペアリング受付、共通実装)。Pages の「血圧計を再ペアリング」から届く |
//! | `OMRON BP ON\|OFF` / `OMRON STATUS` | 血圧計 (HEM-6231T) を拾うか (共通実装、既定 OFF)。**ON にしたら再起動が要る** — main.rs 冒頭の「血圧計」節 |
//! | `VEIN CAPTURE` | 指静脈を読む (`vein` feature)。`VEIN CHARA <hex>` / `ERR VEIN <reason>`。feature 無しは共通実装が `ERR VEIN: unsupported` |
//! | `VEIN SAY PLACE\|AGAIN\|ENROLLED\|FAILED` | 案内音声 (`vein` feature)。`OK VEIN SAY <x>` |

use alc_hub_common::{
    config,
    control::PairFlag,
    settings::Settings,
    status::{epoch_ms, SharedStatus},
};
use alc_hub_core::protocol::{parse_line, HostCommand, HostKind};
use alc_hub_core::uplink::MIN_SYNCED_MS;
use alc_hub_drivers::console;
#[cfg(feature = "vein")]
use alc_hub_drivers::vein;
use anyhow::Result;

pub fn start(
    status: SharedStatus,
    settings: Settings,
    pair_flag: PairFlag,
    #[cfg(feature = "vein")] vein: vein::Link,
) -> Result<()> {
    console::spawn_reader(c"console", 8 * 1024, move |line| {
        handle_line(
            line,
            &status,
            &settings,
            &pair_flag,
            #[cfg(feature = "vein")]
            &vein,
        )
    })
}

fn handle_line(
    line: &str,
    status: &SharedStatus,
    settings: &Settings,
    pair_flag: &PairFlag,
    #[cfg(feature = "vein")] vein: &vein::Link,
) {
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
    let Some(command) = console::handle_common(command, status, settings, HostKind::Timecard)
    else {
        return;
    };
    // 血圧計 (HEM-6231T) の設定も共通実装へ (CoreS3 の host_link と同じ口)。
    // **本機は BLE を起動時の設定で立てる**ので、OFF → ON の反映には再起動が
    // 要る (main.rs 冒頭の「血圧計」節)
    let Some(command) = console::handle_omron(command, status, settings) else {
        return;
    };
    // 血圧計の再ペアリング要求も共通実装へ (Pages の「血圧計を再ペアリング」)。
    // 本機の BLE が立っていない (`OMRON BP OFF`) ときはフラグが消費されないだけ
    let Some(command) = console::handle_pair(command, pair_flag) else {
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
        // 指静脈 (Vein Station)。読み取りの結果は vein スレッドが後から 1 行で出す
        #[cfg(feature = "vein")]
        HostCommand::VeinCapture => vein.capture(),
        #[cfg(feature = "vein")]
        HostCommand::VeinSay(voice) => vein.say(voice),
        // 本機で意味を持たないコマンド (画面遷移 / 印刷 / BLE / Wi-Fi / CFG 等)
        other => {
            log::debug!("console: unsupported command: {other:?}");
            println!("ERR UNSUPPORTED ({})", HostKind::Timecard.label());
        }
    }
}
