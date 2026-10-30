//! Vein Base を警告デバイスとして使うときの配線 (`vein` feature)。
//!
//! **判定はここに書かない。** 沈黙・NG・呼び出しの状態機械は `alc_hub_core::alarm`、
//! 音とリングへの記録は `alc_hub_drivers::alarm` で、VoiceS3R (`atoms3-alarm`) と
//! CoreS3 と同じ関数を通る。ここがやるのは判定器の生成と、50ms ごとの `tick` と
//! 本体ボタンの読みだけ (main.rs 冒頭の「警告デバイスを兼ねる」節)。

use std::sync::{mpsc::Sender, Arc, Mutex};

use alc_hub_common::status::now_ms;
use alc_hub_core::alarm::{AlarmMonitor, SharedMonitor, SILENCE_MS};
use alc_hub_drivers::{alarm, speaker::Sound};
use anyhow::Result;
use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{Gpio41, PinDriver, Pull},
};

/// 鳴動ループの周期。VoiceS3R・CoreS3 と同じ 50ms
const TICK_MS: u32 = 50;

/// ボタンの読みが何サンプル続いたら確定とみなすか (50ms × 3 = 150ms、VoiceS3R と同じ)
const BUTTON_DEBOUNCE_SAMPLES: u8 = 3;

/// 判定器を作る。**武装方式** (`with_boot_grace(None)`) — 初回 heartbeat まで鳴らない。
/// USB/JTAG 起因の reset で前回が武装済みなら武装済みで戻す (CoreS3 と同じ分岐、#194)
pub fn new_monitor(reset_code: i32) -> SharedMonitor {
    let monitor = if alc_hub_core::crashlog::is_usb_serial_reset(reset_code)
        && alarm::restore_armed_flag()
    {
        alc_hub_common::evtlog::emit(&format!("EVT ALARM_RESTORED reset={reset_code}"));
        AlarmMonitor::with_boot_grace(Some(SILENCE_MS))
    } else {
        AlarmMonitor::with_boot_grace(None)
    };
    Arc::new(Mutex::new(monitor))
}

/// 鳴動ループを立てる。main のループは 100ms で回り UiCommand の読み捨てを持つので、
/// 別スレッドにする (CoreS3 と同じ形)。
///
/// `emit_lines = false` — Vein のポートへ `EVT ALARM` を出さない。状態は `STATUS` の
/// `ALARM=…` で渡す (console.rs)
pub fn start(
    monitor: SharedMonitor,
    speaker: Option<Sender<Sound>>,
    button_pin: Gpio41<'static>,
) -> Result<()> {
    // 本体ボタン G41 (active-low)。内部プルアップは `input` の第 2 引数で入れる
    let button = PinDriver::input(button_pin, Pull::Up)?;
    alc_hub_drivers::task::name_next(c"alarm");
    std::thread::Builder::new()
        .name("alarm".into())
        .stack_size(4 * 1024)
        .spawn(move || {
            let mut raw_pressed = false;
            let mut streak: u8 = 0;
            let mut pressed = false;
            loop {
                FreeRtos::delay_ms(TICK_MS);
                let now = now_ms();

                // デバウンス: 同じ読みが続いたときだけ確定を動かす (VoiceS3R と同じ)
                let level = button.is_low();
                if level == raw_pressed {
                    streak = streak.saturating_add(1);
                } else {
                    raw_pressed = level;
                    streak = 1;
                }
                let mut button_edge = false;
                if streak >= BUTTON_DEBOUNCE_SAMPLES && pressed != raw_pressed {
                    pressed = raw_pressed;
                    button_edge = pressed;
                }

                // lock 失敗 (コンソールスレッドが panic した) なら判定は進められない。
                // ログだけ残してループは回し続ける
                let mut actions = Vec::new();
                match monitor.lock() {
                    Ok(mut m) => {
                        if button_edge {
                            actions.extend(m.on_button(now));
                        }
                        actions.extend(m.tick(now));
                    }
                    Err(e) => log::error!("alarm: monitor の lock に失敗: {e}"),
                }
                alarm::run_actions(actions, &speaker, false);
            }
        })?;
    Ok(())
}
