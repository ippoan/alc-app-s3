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
//!   ├─ BLE 測定受信 (待機中のみ) → Temperature / BloodPressure ─タップ/30秒→ Idle
//!   └─ ホストコマンド: QR / MEASURE / RESULT / ERROR / RESET は従来どおり
//!
//! 点呼 (Measuring) 中の BLE 測定・ホスト RESULT は画面遷移せず、同一画面の
//! 体温 (上段) / 血圧 (中段) / アルコール (最下段) の欄を直接更新する。
//! BLE 接続開始 (BleAcquiring) でラベル横にスピナーを表示し、どちらを
//! 取得中かを示す。体温+血圧が揃ってから TENKO_DONE_CLOSE_MS (5秒) で
//! 待機画面へ戻る (アルコールは表示のみ — 完了条件は運用ごとに異なるため
//! 今後実装)。無操作時は TENKO_TIMEOUT_MS (長め) で待機画面へ戻る。
//! ```
//!
//! コマンドは host_link (USB CDC) と ble から mpsc 経由で届く。描画は状態
//! 変化時の全画面再描画 + 部分更新 (時計 / QR 残り秒数 / スピナー)。

mod screens;

use std::sync::mpsc::Receiver;

use alc_hub_core::device::DeviceKind;
use alc_hub_core::layout::map_touch;
use alc_hub_board::{
    display::{self, Cs3Display, LCD_H, LCD_W},
    touch,
};
use alc_hub_common::{
    config,
    status::{now_ms, SharedStatus},
};
use esp_idf_svc::hal::{delay::FreeRtos, i2c::I2cDriver};

// コマンド定義は I/O 層 (host_link / ble が送信側) と共有
pub use alc_hub_common::ui_api::UiCommand;

pub(crate) enum Screen {
    /// 待機画面 (NFC カード待ち)
    Idle,
    /// メニュー (上: 点呼 / 下: ログ確認)
    Menu,
    Qr {
        payload: String,
        timeout_ms: u64,
    },
    /// 点呼: 体温 / 血圧 / アルコールを同一画面で計測・確認する (3 段表示)
    Measuring {
        /// 体温 (℃)。None = 未計測
        temp: Option<f32>,
        /// 血圧 (収縮期, 拡張期, 脈拍)。None = 未計測
        bp: Option<(f32, f32, Option<f32>)>,
        /// アルコール測定結果 (ok, 表示値)。ホストの RESULT で更新。
        /// 表示のみで点呼完了条件には含めない (対面点呼など運用により
        /// アルコールチェッカーの扱いが異なるため — 今後実装)
        alcohol: Option<(bool, String)>,
        /// 体温+血圧が揃った時刻 [ms]。TENKO_DONE_CLOSE_MS 経過で待機画面へ
        done_at: Option<u64>,
    },
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
    /// auth-worker デバイス登録の承認待ち (user_code + 承認 URL の QR)
    Pairing {
        user_code: String,
        url: String,
        timeout_ms: u64,
    },
    /// auth-worker デバイス登録の結果
    PairingResult {
        ok: bool,
        message: String,
    },
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
    // BLE で取得中の機器 (点呼画面のラベル横スピナー表示)。
    // 接続開始 (BleAcquiring) で設定し、切断/再スキャン (BleIdle) で解除
    let mut acquiring: Option<DeviceKind> = None;

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
                    dirty = true;
                }
                // 点呼中は画面遷移せず、点呼画面の体温/血圧欄を直接更新する。
                // それ以外のバイタル自動表示は待機画面 (または既にバイタル
                // 表示中) のみ。QR・メニュー等の操作中に不意の画面遷移を
                // させない (測定値はログとホストへの JSON 出力には常に残る)
                UiCommand::Temperature { celsius } => {
                    if let Screen::Measuring {
                        temp, bp, done_at, ..
                    } = &mut screen
                    {
                        *temp = Some(celsius);
                        if bp.is_some() && done_at.is_none() {
                            *done_at = Some(now);
                        }
                        if acquiring == Some(DeviceKind::Thermometer) {
                            acquiring = None;
                        }
                        dirty = true;
                    } else if vitals_display_allowed(&screen) {
                        screen = Screen::Temperature { celsius };
                        entered = now;
                        dirty = true;
                    } else {
                        log::info!("ui: 体温表示を抑制 (操作中の画面を優先)");
                    }
                }
                UiCommand::BloodPressure {
                    systolic,
                    diastolic,
                    pulse,
                } => {
                    if let Screen::Measuring {
                        temp, bp, done_at, ..
                    } = &mut screen
                    {
                        *bp = Some((systolic, diastolic, pulse));
                        if temp.is_some() && done_at.is_none() {
                            *done_at = Some(now);
                        }
                        if acquiring == Some(DeviceKind::BloodPressure) {
                            acquiring = None;
                        }
                        dirty = true;
                    } else if vitals_display_allowed(&screen) {
                        screen = Screen::BloodPressure {
                            systolic,
                            diastolic,
                            pulse,
                        };
                        entered = now;
                        dirty = true;
                    } else {
                        log::info!("ui: 血圧表示を抑制 (操作中の画面を優先)");
                    }
                }
                // BLE 接続開始/終了: 点呼画面のスピナー表示状態のみ更新。
                // 再描画はスピナーを実際に描く/消す場合のみ — 値が入って
                // いる項目は回さないため、送信済み機器への空接続 (hub-ble
                // 参照) では画面を触らず、ちらつきを防ぐ
                UiCommand::BleAcquiring { device } => {
                    acquiring = Some(device);
                    if tenko_spinner_visible(&screen, device) {
                        dirty = true;
                    }
                }
                UiCommand::BleIdle => {
                    if let Some(kind) = acquiring.take() {
                        if tenko_spinner_visible(&screen, kind) {
                            dirty = true;
                        }
                    }
                }
                // 点呼中の RESULT はアルコール欄の更新のみ (画面遷移しない)。
                // それ以外は従来どおり結果画面へ
                UiCommand::Result { ok, value } => {
                    if let Screen::Measuring { alcohol, .. } = &mut screen {
                        *alcohol = Some((ok, value));
                        dirty = true;
                    } else {
                        screen = Screen::Result { ok, value };
                        entered = now;
                        dirty = true;
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
                        UiCommand::Measure => Screen::Measuring {
                            temp: None,
                            bp: None,
                            alcohol: None,
                            done_at: None,
                        },
                        UiCommand::Error { message } => Screen::Error { message },
                        UiCommand::Reset => Screen::Idle,
                        UiCommand::ShowPairing {
                            user_code,
                            url,
                            timeout_ms,
                        } => Screen::Pairing {
                            user_code,
                            url,
                            timeout_ms,
                        },
                        UiCommand::PairingResult { ok, message } => {
                            Screen::PairingResult { ok, message }
                        }
                        UiCommand::Rotate(_)
                        | UiCommand::Temperature { .. }
                        | UiCommand::BloodPressure { .. }
                        | UiCommand::BleAcquiring { .. }
                        | UiCommand::BleIdle
                        | UiCommand::Result { .. } => unreachable!(),
                    };
                    entered = now;
                    dirty = true;
                }
            }
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
            // 点呼: 体温・血圧の両方が揃ったら 5 秒表示して待機画面へ
            Screen::Measuring {
                done_at: Some(done),
                ..
            } if now.saturating_sub(*done) > config::TENKO_DONE_CLOSE_MS => {
                println!("EVT TENKO_DONE");
                true
            }
            // 点呼: 測定が揃わないまま長時間経過したら待機画面へ (長め)
            Screen::Measuring { .. } if elapsed > config::TENKO_TIMEOUT_MS => {
                println!("EVT TENKO_TIMEOUT");
                true
            }
            Screen::Temperature { .. } | Screen::BloodPressure { .. }
                if elapsed > config::VITALS_AUTO_CLOSE_MS =>
            {
                true
            }
            // 登録承認待ち: pairing の有効期限で閉じる (結果は auth_link が
            // PairingResult で別途通知するため、ここでは画面を畳むだけ)
            Screen::Pairing { timeout_ms, .. } if elapsed > *timeout_ms => true,
            Screen::PairingResult { .. } if elapsed > config::PAIRING_RESULT_CLOSE_MS => true,
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
                if let Screen::Qr { timeout_ms, .. } | Screen::Pairing { timeout_ms, .. } =
                    &screen
                {
                    let remain_s = timeout_ms.saturating_sub(now.saturating_sub(entered)) / 1000;
                    screens::draw_qr_countdown(&mut display, remain_s);
                }
                last_bar = now;
            }
            // 点呼画面: BLE 取得中の機器ラベル横スピナーをアニメーション
            // (未取得の項目のみ — tenko_spinner_visible 参照)
            if let Some(kind) = acquiring {
                if tenko_spinner_visible(&screen, kind)
                    && now.saturating_sub(last_spin) >= 150
                {
                    spin_phase = (spin_phase + 1) % 8;
                    screens::draw_tenko_spinner(&mut display, kind, spin_phase);
                    last_spin = now;
                }
            }
        }

        FreeRtos::delay_ms(20);
    }
}

/// 点呼画面で kind のスピナーを描くべきか — 未取得の項目のみ。
/// 値が入っている項目で回すと、送信済み機器へのデータなし空接続 (hub-ble
/// 参照) のたびに「取得中」に見えてしまう。測り直しの値は表示だけ更新される
fn tenko_spinner_visible(screen: &Screen, kind: DeviceKind) -> bool {
    match screen {
        Screen::Measuring { temp, bp, .. } => match kind {
            DeviceKind::Thermometer => temp.is_none(),
            DeviceKind::BloodPressure => bp.is_none(),
        },
        _ => false,
    }
}

/// バイタル (体温/血圧) の自動表示 (画面遷移) を許可する画面か。
///
/// - 待機中・バイタル表示中: 表示する (連続測定は表示を更新)
/// - 点呼の測定待ち (Measuring): ここには来ない — 画面遷移せず点呼画面内の
///   体温/血圧欄を直接更新する (コマンド処理側で分岐)
/// - QR / メニュー / ログ / 結果 / エラー: 奪わない (不意の遷移防止)
fn vitals_display_allowed(screen: &Screen) -> bool {
    matches!(
        screen,
        Screen::Idle | Screen::Temperature { .. } | Screen::BloodPressure { .. }
    )
}

/// タップ時の画面遷移先 (None = 変化なし)。y は回転補正済みの論理座標。
fn on_click(screen: &Screen, y: i32, logical_h: i32) -> Option<Screen> {
    match screen {
        Screen::Idle => Some(Screen::Menu),
        Screen::Menu => {
            if y < logical_h / 2 {
                // 点呼開始をホストへ通知し、体温/血圧/アルコールの測定待ちへ
                println!("EVT TENKO_START");
                Some(Screen::Measuring {
                    temp: None,
                    bp: None,
                    alcohol: None,
                    done_at: None,
                })
            } else {
                Some(Screen::Log)
            }
        }
        Screen::Log
        | Screen::Measuring { .. }
        | Screen::Result { .. }
        | Screen::Error { .. }
        | Screen::Temperature { .. }
        | Screen::BloodPressure { .. }
        | Screen::PairingResult { .. } => Some(Screen::Idle),
        // QR / 登録承認待ちは誤タップで閉じない (タイムアウト・結果通知のみ)
        Screen::Qr { .. } | Screen::Pairing { .. } => None,
    }
}
