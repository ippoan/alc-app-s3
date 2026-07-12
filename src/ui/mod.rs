//! 画面処理: 状態機械と UI ループ。
//!
//! 画面遷移:
//!
//! ```text
//! Boot ─→ Idle ─(QR cmd)─→ Qr ─(MEASURE)─→ Measuring ─(RESULT)─→ Result ─┐
//!          ↑                │timeout → EVT QR_TIMEOUT            自動/タップ│
//!          ├────────────────┴───────────────────────────────────────────┘
//!          ├─(タップ)─→ StatusDetail ─(タップ)─→ Idle
//!          └─(ERROR cmd はどの画面からでも)─→ Error ─(タップ/RESET)─→ Idle
//! ```
//!
//! コマンドは host_link (USB CDC) から mpsc 経由で届く。描画は状態変化時の
//! 全画面再描画 + 部分更新 (ステータスバー毎秒 / QR 残り秒数 / スピナー)。

mod screens;

use std::sync::mpsc::Receiver;

use esp_idf_svc::hal::{delay::FreeRtos, i2c::I2cDriver};

use crate::{
    board::{
        display::{self, Cs3Display},
        touch,
    },
    config,
    status::{now_ms, SharedStatus},
};

/// ホストからの画面操作コマンド
pub enum UiCommand {
    ShowQr { payload: String, timeout_ms: u64 },
    Measure,
    Result { ok: bool, value: String },
    Error { message: String },
    Reset,
    /// 画面向き変更 (0/90/180/270 度)。NVS への保存は host_link 側で実施済み
    Rotate(u16),
}

enum Screen {
    Idle,
    Qr { payload: String, timeout_ms: u64 },
    Measuring,
    Result { ok: bool, value: String },
    Error { message: String },
    StatusDetail,
}

pub fn run(
    mut display: Cs3Display,
    mut i2c: I2cDriver<'static>,
    rx: Receiver<UiCommand>,
    status: SharedStatus,
) -> ! {
    screens::draw_boot(&mut display);

    let mut screen = Screen::Idle;
    let mut entered = now_ms();
    let mut dirty = true;
    let mut last_bar = 0u64;
    let mut last_spin = 0u64;
    let mut spin_phase = 0u8;
    let mut touching = false;

    loop {
        let now = now_ms();

        // --- ホストコマンド ---
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                // 画面向き変更は現在の画面を維持したまま再描画のみ
                UiCommand::Rotate(deg) => {
                    if let Err(e) =
                        display.set_orientation(display::orientation_from_deg(deg))
                    {
                        log::warn!("ui: 画面向き変更失敗: {e:?}");
                    }
                }
                cmd => {
                    screen = match cmd {
                        UiCommand::ShowQr {
                            payload,
                            timeout_ms,
                        } => Screen::Qr {
                            payload,
                            timeout_ms,
                        },
                        UiCommand::Measure => Screen::Measuring,
                        UiCommand::Result { ok, value } => Screen::Result { ok, value },
                        UiCommand::Error { message } => Screen::Error { message },
                        UiCommand::Reset => Screen::Idle,
                        UiCommand::Rotate(_) => unreachable!(),
                    };
                    entered = now;
                }
            }
            dirty = true;
        }

        // --- 自動遷移 ---
        match &screen {
            Screen::Qr { timeout_ms, .. } if now.saturating_sub(entered) > *timeout_ms => {
                println!("EVT QR_TIMEOUT");
                screen = Screen::Idle;
                entered = now;
                dirty = true;
            }
            Screen::Result { .. }
                if now.saturating_sub(entered) > config::RESULT_AUTO_CLOSE_MS =>
            {
                println!("EVT RESULT_CLOSED");
                screen = Screen::Idle;
                entered = now;
                dirty = true;
            }
            _ => {}
        }

        // --- タッチ (離した瞬間をクリックとする) ---
        let t = touch::read(&mut i2c);
        if touching && t.is_none() {
            if let Some(next) = on_click(&screen) {
                screen = next;
                entered = now;
                dirty = true;
            }
        }
        touching = t.is_some();

        // --- 描画 ---
        if dirty {
            let st = status.lock().map(|s| s.clone()).unwrap_or_default();
            screens::draw_full(&mut display, &screen, &st, now, entered);
            last_bar = now;
            last_spin = now;
            dirty = false;
        } else {
            if now.saturating_sub(last_bar) >= 1000 {
                let st = status.lock().map(|s| s.clone()).unwrap_or_default();
                screens::draw_status_bar(&mut display, &st, now);
                if let Screen::Qr { timeout_ms, .. } = &screen {
                    let remain_s =
                        timeout_ms.saturating_sub(now.saturating_sub(entered)) / 1000;
                    screens::draw_qr_countdown(&mut display, remain_s);
                }
                last_bar = now;
            }
            if matches!(screen, Screen::Measuring) && now.saturating_sub(last_spin) >= 150 {
                spin_phase = (spin_phase + 1) % 8;
                screens::draw_spinner(&mut display, spin_phase);
                last_spin = now;
            }
        }

        FreeRtos::delay_ms(20);
    }
}

/// タップ時の画面遷移先 (None = 変化なし)
fn on_click(screen: &Screen) -> Option<Screen> {
    match screen {
        Screen::Idle => Some(Screen::StatusDetail),
        Screen::StatusDetail | Screen::Result { .. } | Screen::Error { .. } => {
            Some(Screen::Idle)
        }
        // QR / 測定中は誤タップで閉じない (ホストの RESET / タイムアウトのみ)
        Screen::Qr { .. } | Screen::Measuring => None,
    }
}
