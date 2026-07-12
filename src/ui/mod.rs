//! 画面処理: 状態機械と UI ループ。
//!
//! タッチ主導のキオスクフロー:
//!
//! ```text
//!            ┌─(上半分タップ)→ Measuring(点呼) ─(RESULT cmd)→ Result ─┐
//! Idle ─タップ→ Menu                                          自動/タップ│
//! (NFC待機)  └─(下半分タップ)→ Log ─タップ→ Idle                      │
//!   ↑  ↑                                                              │
//!   │  └──────────────────────────────────────────────────────────────┘
//!   ├─ BLE 測定受信 → Temperature / BloodPressure ─タップ/30秒→ Idle
//!   └─ ホストコマンド: QR / MEASURE / RESULT / ERROR / RESET は従来どおり
//! ```
//!
//! コマンドは host_link (USB CDC) と ble から mpsc 経由で届く。描画は状態
//! 変化時の全画面再描画 + 部分更新 (時計 / QR 残り秒数 / スピナー)。

mod screens;

use std::sync::mpsc::Receiver;

use alc_hub_core::layout::map_touch;
use esp_idf_svc::hal::{delay::FreeRtos, i2c::I2cDriver};

use crate::{
    board::{
        display::{self, Cs3Display, LCD_H, LCD_W},
        touch,
    },
    config,
    status::{now_ms, SharedStatus},
};

/// ホスト / BLE からの画面操作コマンド
pub enum UiCommand {
    ShowQr {
        payload: String,
        timeout_ms: u64,
    },
    Measure,
    Result {
        ok: bool,
        value: String,
    },
    Error {
        message: String,
    },
    Reset,
    /// 画面向き変更 (0/90/180/270 度)。NVS への保存は host_link 側で実施済み
    Rotate(u16),
    /// BLE 体温計の測定値 (℃)
    Temperature {
        celsius: f32,
    },
    /// BLE 血圧計の測定値 (mmHg)
    BloodPressure {
        systolic: f32,
        diastolic: f32,
        pulse: Option<f32>,
    },
}

pub(crate) enum Screen {
    /// 待機画面 (NFC カード待ち)
    Idle,
    /// メニュー (上: 点呼 / 下: ログ確認)
    Menu,
    Qr {
        payload: String,
        timeout_ms: u64,
    },
    Measuring,
    Result {
        ok: bool,
        value: String,
    },
    Error {
        message: String,
    },
    /// 体温表示 (BLE)
    Temperature {
        celsius: f32,
    },
    /// 血圧表示 (BLE)
    BloodPressure {
        systolic: f32,
        diastolic: f32,
        pulse: Option<f32>,
    },
    /// イベントログ + 機器ステータス
    Log,
}

pub fn run(
    mut display: Cs3Display,
    mut i2c: I2cDriver<'static>,
    rx: Receiver<UiCommand>,
    status: SharedStatus,
    initial_rotation: u16,
) -> ! {
    screens::draw_boot(&mut display);

    let mut rotation = initial_rotation;
    let mut screen = Screen::Idle;
    let mut entered = now_ms();
    let mut dirty = true;
    let mut last_bar = 0u64;
    let mut last_spin = 0u64;
    let mut spin_phase = 0u8;
    let mut last_touch: Option<touch::TouchPoint> = None;

    loop {
        let now = now_ms();

        // --- コマンド (ホスト / BLE) ---
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                // 画面向き変更は現在の画面を維持したまま再描画のみ
                UiCommand::Rotate(deg) => {
                    if let Err(e) = display.set_orientation(display::orientation_from_deg(deg)) {
                        log::warn!("ui: 画面向き変更失敗: {e:?}");
                    }
                    rotation = deg;
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
                        UiCommand::Temperature { celsius } => Screen::Temperature { celsius },
                        UiCommand::BloodPressure {
                            systolic,
                            diastolic,
                            pulse,
                        } => Screen::BloodPressure {
                            systolic,
                            diastolic,
                            pulse,
                        },
                        UiCommand::Rotate(_) => unreachable!(),
                    };
                    entered = now;
                }
            }
            dirty = true;
        }

        // --- 自動遷移 ---
        let elapsed = now.saturating_sub(entered);
        let auto_close = match &screen {
            Screen::Qr { timeout_ms, .. } if elapsed > *timeout_ms => {
                println!("EVT QR_TIMEOUT");
                true
            }
            Screen::Result { .. } if elapsed > config::RESULT_AUTO_CLOSE_MS => {
                println!("EVT RESULT_CLOSED");
                true
            }
            Screen::Temperature { .. } | Screen::BloodPressure { .. }
                if elapsed > config::VITALS_AUTO_CLOSE_MS =>
            {
                true
            }
            _ => false,
        };
        if auto_close {
            screen = Screen::Idle;
            entered = now;
            dirty = true;
        }

        // --- タッチ (離した瞬間をクリックとする) ---
        let t = touch::read(&mut i2c);
        if let Some(p) = &t {
            last_touch = Some(*p);
        } else if let Some(p) = last_touch.take() {
            let (_, y) = map_touch(i32::from(p.x), i32::from(p.y), rotation, LCD_W, LCD_H);
            let logical_h = if rotation == 90 || rotation == 270 {
                LCD_W
            } else {
                LCD_H
            };
            if let Some(next) = on_click(&screen, y, logical_h) {
                screen = next;
                entered = now;
                dirty = true;
            }
        }

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
                // 時計・インジケータのみの部分更新 (全面クリアしない — blink 防止)
                screens::update_status_bar(&mut display, &st, now);
                if let Screen::Qr { timeout_ms, .. } = &screen {
                    let remain_s = timeout_ms.saturating_sub(now.saturating_sub(entered)) / 1000;
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

/// タップ時の画面遷移先 (None = 変化なし)。y は回転補正済みの論理座標。
fn on_click(screen: &Screen, y: i32, logical_h: i32) -> Option<Screen> {
    match screen {
        Screen::Idle => Some(Screen::Menu),
        Screen::Menu => {
            if y < logical_h / 2 {
                // 点呼開始をホストへ通知し、FC-1200 の測定待ちへ
                println!("EVT TENKO_START");
                Some(Screen::Measuring)
            } else {
                Some(Screen::Log)
            }
        }
        Screen::Log
        | Screen::Measuring
        | Screen::Result { .. }
        | Screen::Error { .. }
        | Screen::Temperature { .. }
        | Screen::BloodPressure { .. } => Some(Screen::Idle),
        // QR は誤タップで閉じない (ホストの RESET / タイムアウトのみ)
        Screen::Qr { .. } => None,
    }
}
