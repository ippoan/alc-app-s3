//! 警告デバイスの配線 — 判定器 ([`alc_hub_core::alarm::AlarmMonitor`]) を共有し、
//! 返ってきた [`Action`] を実行する層 (issue #135 / #187)。
//!
//! **判定はここに書かない。** 沈黙・NG・呼び出しの状態機械はホストでテスト済みの
//! 純粋ロジック (`alc_hub_core::alarm`) が持つ。ここがやるのは
//!
//! - 判定器を lock して現在時刻とともに渡す ([`with_monitor`] / [`apply_heartbeat`])
//! - [`Action`] を音とホストへの行に変換する ([`run_actions`]、`speaker` feature)
//!
//! の 2 つだけ。**機種ごとに書き写さないこと** — VoiceS3R (`atoms3-alarm`) と
//! CoreS3 (root の `alc-hub-cores3`) は同じ関数を通る。片方だけ直ると
//! 「実機では鳴り方が違う」が起きる。
//!
//! # 行を出すか出さないかは機種で違う (`emit_lines`)
//!
//! VoiceS3R は `EVT ALARM …` をキオスクへ出す (ブラウザのバナー用)。**CoreS3 は
//! 出さない** — ブラウザ側 (`useCoreS3Serial` の `classify()`) は行頭
//! `EVT ALARM` / `STATUS alarm` を「警告デバイス = 別機種」と判定し、CoreS3 の
//! ポートを reject する。CoreS3 は代わりに既存 `STATUS LAN=… BOARD=cores3` 行の
//! 末尾へ [`AlarmMonitor::status_field`] を足して伝える (issue #187)。

//!
//! # 武装状態は USB/JTAG reset を跨いで残す (`ARMED`, issue #194)
//!
//! 運行者 PC の PWA タブを閉じると Windows の driver がハンドル解放で DTR → RTS を
//! 落とし、途中の「DTR=0 かつ RTS=1」で USB-Serial-JTAG がチップを reset する。
//! ブラウザ側 (ippoan/alc-app#190 / #200) では防げなかった。再起動で
//! [`AlarmMonitor`] の「初回 heartbeat で武装」が消えると**閉じても鳴らない**ので、
//! heartbeat を受けるたびに `.noinit` の [`ArmedFlag`] へ「武装済み」を書き、
//! 起動側 (各 bin の main) が **USB/JTAG 起因の reset かつ magic 一致** のときだけ
//! [`restore_armed_flag`] で拾って武装済み (`with_boot_grace(Some(SILENCE_MS))`)
//! で生成する。電源断でゴミになるのは magic 不一致で弾く (crashlog の `RING` と
//! 同じ自己修復の形)。解除経路は元々無いので、クリアもしない。

use core::mem::MaybeUninit;

use alc_hub_core::alarm::{AlarmMonitor, SharedMonitor};

/// "ARMD" — `.noinit` の [`ArmedFlag`] が前回稼働から保持されているかの判定 magic。
const ARMED_MAGIC: u32 = 0x41524d44;

/// `.noinit` に置く武装フラグ。crashlog の `Ring` とは責務が違うので別の静的領域
/// (RING の構造体に足さない)。8 バイトの DRAM を常時消費する
#[repr(C)]
struct ArmedFlag {
    magic: u32,
    /// 1 = 武装済み (heartbeat を一度でも受けた)
    armed: u8,
}

#[link_section = ".noinit"]
static mut ARMED: MaybeUninit<ArmedFlag> = MaybeUninit::uninit();

fn armed_ptr() -> *mut ArmedFlag {
    // MaybeUninit<ArmedFlag> は ArmedFlag と同一レイアウト。u8/u32 は全ビット
    // パターンが有効なため電源断後のゴミも「読める」— 信頼性は magic で判定する
    core::ptr::addr_of_mut!(ARMED) as *mut ArmedFlag
}

/// 武装済みを `.noinit` に記録する。heartbeat のたびに呼ぶ (RAM への代入なので
/// 毎回でよい)。書き手は heartbeat の受け手 1 スレッドだけで、値も単調
/// (未武装 → 武装) なので排他は要らない
fn store_armed_flag() {
    unsafe {
        let f = armed_ptr();
        (*f).armed = 1;
        (*f).magic = ARMED_MAGIC;
    }
}

/// 前回稼働の武装状態を `.noinit` から読む。`true` = magic が一致し武装済み。
///
/// **呼び出し側が reset 理由を見てから使うこと** — sw (esp_restart) や電源投入では
/// 復元しない (`alc_hub_core::crashlog::is_usb_serial_reset`)。電源投入直後の
/// ゴミは magic 不一致で `false`
pub fn restore_armed_flag() -> bool {
    unsafe {
        let f = armed_ptr();
        (*f).magic == ARMED_MAGIC && (*f).armed == 1
    }
}

/// 判定器を lock して現在時刻とともに渡す。
///
/// **lock 失敗時に黙って捨てない** — heartbeat を落とすと沈黙とみなされて
/// 鳴り出すので、なぜ落としたかがログに残っている必要がある
pub fn with_monitor(monitor: &SharedMonitor, f: impl FnOnce(&mut AlarmMonitor, u64)) {
    match monitor.lock() {
        Ok(mut m) => {
            let now = alc_hub_common::status::now_ms();
            f(&mut m, now)
        }
        Err(e) => log::error!("alarm: monitor の lock に失敗: {e}"),
    }
}

/// `HB OK` / `HB NG <reason>` (末尾 `call=0|1` / `grace=<秒>`) を判定器へ渡す。
///
/// **応答を返さない** — 3 秒ごとに来るので返すとホストのログが埋まる。
/// 状態遷移もここでは起こさず、次の `tick` (鳴動ループ) がまとめて判定する。
/// `grace` は意図した reload の直前にブラウザが付ける 1 回限りの猶予 (issue #192)
pub fn apply_heartbeat(
    monitor: &SharedMonitor,
    ok: bool,
    reason: Option<&str>,
    call: bool,
    grace: Option<u16>,
) {
    // 受けた = 武装した。USB/JTAG reset を跨いで残す (モジュール doc、#194)
    store_armed_flag();
    with_monitor(monitor, |m, now| {
        m.on_heartbeat_with_grace(now, ok, reason, call, grace)
    });
}

#[cfg(feature = "speaker")]
mod player {
    use crate::speaker::Sound;
    use alc_hub_core::alarm::Action;
    use std::sync::mpsc::Sender;

    /// 判定器が返した [`Action`] を実行する (音を鳴らす / ホストへ行を出す)。
    ///
    /// `speaker` が `None` (初期化に失敗した個体) なら音は捨てる。
    /// `emit_lines` が `false` なら `Action::Emit` の行も捨てる (CoreS3。
    /// モジュール doc の「行を出すか出さないか」参照)
    pub fn run_actions(
        actions: impl IntoIterator<Item = Action>,
        speaker: &Option<Sender<Sound>>,
        emit_lines: bool,
    ) {
        for action in actions {
            match action {
                // 鳴らし直しの周期は判定器が刻む (alarm::ALERT_PERIOD_MS)。
                // **再生スレッド側でループを作らない** — 占有するとボタンで
                // 止めたのに鳴り続ける (speaker.rs の Sound::Alert の doc)
                Action::PlayAlert => send(speaker, Sound::Alert),
                Action::PlayResolved => send(speaker, Sound::AlertResolved),
                // 黙らせているあいだの短い合図 (alarm::MUTED_TICK_MS ごと)。
                // 完全な無音だと異常が続いていることを忘れられる
                Action::PlayMutedTick => send(speaker, Sound::MutedTick),
                // 沈黙 (繋がっていない) のときの短い 2 連 (alarm::SILENCE_TICK_MS ごと)
                Action::PlaySilenceTick => send(speaker, Sound::SilenceTick),
                // キオスクのバナー用。遷移のたび + BANNER_MS ごとに出る
                Action::Emit(line) => {
                    if emit_lines {
                        println!("{line}");
                    }
                }
            }
        }
    }

    /// 再生依頼をキューへ積む。**ここで待たない** — I2S の write はブロッキングで、
    /// 直接鳴らすと鳴動ループが 1 秒近く止まりボタンの反映が遅れる
    fn send(speaker: &Option<Sender<Sound>>, sound: Sound) {
        if let Some(tx) = speaker {
            let _ = tx.send(sound);
        }
    }
}

#[cfg(feature = "speaker")]
pub use player::run_actions;
