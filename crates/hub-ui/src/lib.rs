//! 画面処理: 状態機械と UI ループ。
//!
//! タッチ主導のキオスクフロー:
//!
//! ```text
//!            ┌─(上半分タップ)→ Measuring(点呼) ─(RESULT cmd)→ Result ─┐
//! Idle ─タップ→ Menu                                          自動/タップ│
//! (NFC待機)  └─(下半分タップ)→ Log ─タップ→ Idle                      │
//!   ↑  ↑ │                                                            │
//!   │  │ └─免許証タップ→ Confirm ─(上: 点呼を開始)→ Measuring          │
//!   │  │        (下: キャンセル / 15秒放置 → Idle)                     │
//!   │  └──────────────────────────────────────────────────────────────┘
//!   ├─ BLE 測定受信 (待機中のみ) → Temperature / BloodPressure ─タップ/30秒→ Idle
//!   └─ ホストコマンド: QR / MEASURE / RESULT / ERROR / RESET は従来どおり
//!
//! 免許証 (NFC-B) をかざすとメニューを飛ばして点呼確認 (Confirm) へ直行する。
//! かざしてから LOG_LOCK_MS (15 秒) はメニューの「ログ確認」を押せなくする
//! (hub-core tenko_prompt::LogLock)。ロックの残り秒数はメニューの下段ボタンと
//! 待機画面の最下行に出す。点呼確認画面は「点呼を開始」の下に画面自体の残り時間
//! (あと N秒、15 秒で自動クローズ = ロックと同じ長さ) を出す。
//!
//! 点呼 (Measuring) 中の BLE 測定・ホスト RESULT は画面遷移せず、同一画面の
//! 体温 / (血圧) / アルコール の欄を直接更新する。**血圧は運用オプション**
//! (hub-core tenko::TenkoItems、`TENKO BP ON|OFF`、既定 OFF) で、OFF なら段を
//! 出さず体温とアルコールの 2 段にし、血圧計の測定は画面では無視する
//! (ログ・WS 送信には残る)。BLE 接続開始 (BleAcquiring) でラベル横にスピナー
//! を表示し、どちらを取得中かを示す。必須項目 (体温 + アルコール、血圧 ON なら
//! 血圧も) が揃ってから TENKO_DONE_CLOSE_MS (5秒) で待機画面へ戻る。無操作時は
//! TENKO_TIMEOUT_MS (長め) で待機画面へ戻る。
//! ```
//!
//! コマンドは host_link (USB CDC) と ble から mpsc 経由で届く。描画は状態
//! 変化時の全画面再描画 + 部分更新 (時計 / QR 残り秒数 / スピナー)。

mod screens;

use std::sync::mpsc::{Receiver, Sender};

use alc_hub_core::device::DeviceKind;
use alc_hub_core::layout::map_touch;
use alc_hub_core::tenko::TenkoItems;
use alc_hub_core::tenko_prompt::{
    self, ConfirmChoice, ExpiryState, LicenseCard, LogLock, MenuChoice,
};
use alc_hub_board::{
    display::{self, Cs3Display, LCD_H, LCD_W},
    touch,
};
use alc_hub_common::{
    config,
    measurement::Measurement,
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
    /// 点呼: 体温 / (血圧) / アルコールを同一画面で計測・確認する
    Measuring {
        /// 点呼の構成 (血圧を含めるか)。画面に入った時点の設定で固定
        items: TenkoItems,
        /// 体温 (℃)。None = 未計測
        temp: Option<f32>,
        /// 血圧 (収縮期, 拡張期, 脈拍)。None = 未計測。items.bp が false なら
        /// 常に None (血圧計の測定が来ても画面では無視する)
        bp: Option<(f32, f32, Option<f32>)>,
        /// アルコール測定結果 (ok, 表示値)。ホストの RESULT または
        /// FC-1200 (recorder 経由) で更新。点呼完了の必須項目
        alcohol: Option<(bool, String)>,
        /// FC-1200 の測定進行状態 (結果が無い間の「準備中/吹込待ち/判定中」表示)
        alc_stage: Option<alc_hub_common::ui_api::AlcoholStage>,
        /// 必須項目が揃った時刻 [ms] (items.complete)。TENKO_DONE_CLOSE_MS 経過で待機画面へ
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
    /// 免許証タップ後の点呼確認 (上: 点呼を開始 / 下: キャンセル)。
    /// ヘッダに交付日・有効期限 (期限切れなら赤) を出す
    Confirm {
        card: LicenseCard,
        expiry: ExpiryState,
    },
}

/// バッテリー/電源状態 (AXP2101) の取得間隔。診断用なので粗くてよい (Refs #50)。
const BATT_INTERVAL_MS: u64 = 10_000;

pub fn run(
    mut display: Cs3Display,
    mut i2c: I2cDriver<'static>,
    rx: Receiver<UiCommand>,
    status: SharedStatus,
    initial_rotation: u16,
    boot_id: u32,
    meas_tx: Sender<Measurement>,
) -> ! {
    screens::draw_boot(&mut display);

    // UI ループ (メインタスク) を Task WDT に登録する。以降ループ毎に feed し、
    // 描画 / タッチ I2C / status ロックが wedge して feed が 10s 途切れたら
    // esp_task_wdt が chip をリセットする (crashlog が reset_reason=task_wdt で
    // 記録・送信する)。TWDT は sdkconfig で init 済み・idle 監視は無効。
    // 未 init 等で失敗しても UI は続行する (fail-open)。
    alc_hub_common::wdt::subscribe_current_as_ui();

    let mut rotation = initial_rotation;
    // 点呼セッションの発番器 (Refs #112)。**UI だけが発番する** — 点呼の開始と
    // 終了を知っているのはこの状態機械だけで、recorder は HubStatus 経由で
    // 現在値を読むだけ。boot_id は NVS 永続の起動カウンタで、再起動をまたいだ
    // ID の再利用 (= 別々の点呼がサーバ側で融合する) を防ぐ。
    let mut session_gen = alc_hub_core::session::SessionIdGen::new(boot_id);
    let mut in_session = false;
    let mut screen = Screen::Idle;
    let mut entered = now_ms();
    let mut dirty = true;
    let mut last_bar = 0u64;
    let mut last_spin = 0u64;
    let mut last_batt = 0u64;
    let mut spin_phase = 0u8;
    let mut last_touch: Option<touch::TouchPoint> = None;
    // BLE で取得中の機器 (点呼画面のラベル横スピナー表示)。
    // 接続開始 (BleAcquiring) で設定し、切断/再スキャン (BleIdle) で解除
    let mut acquiring: Option<DeviceKind> = None;
    // バックライト減光: 直近の操作 (タッチ・ホスト/BLE コマンド) 時刻。
    // BACKLIGHT_IDLE_DIM_MS 無操作が続くと最低輝度に落とし、操作があれば
    // 即座に復帰する (画面焼け対策の一環)
    let mut last_activity = now_ms();
    let mut backlight_dimmed = false;
    // 免許証タップ後のログ確認ロック (tenko_prompt)。メニューの下段ボタンの
    // 残り秒数表示は 1 秒刻みの部分更新で追従させる (last_lock_secs)
    let mut log_lock = LogLock::new();
    let mut last_lock_secs = 0u64;
    // 点呼確認画面の「点呼を開始」で始めた点呼の札。session_id を発番した直後に
    // Measurement::License として recorder へ渡す (同じ session_id が付く、#125)
    let mut pending_license: Option<LicenseCard> = None;

    loop {
        let now = now_ms();

        // Task WDT feed: このループが回っている = 画面が生きている証跡。
        // wedge して 10s feed が途切れると WDT が chip をリセットする。
        // (OTA 中は ota.rs が pause_ui() で一時停止する、Refs #55)
        alc_hub_common::wdt::feed();

        // バッテリー/電源状態の定期取得 (AXP2101、~10s)。i2c はこのループが
        // 所有しているためここで読む。EVT BATT を USB コンソールへ出し、Log
        // 画面にも反映して「外部給電は来ているのに Core が落ちる (brownout)」
        // 「充電できているか」を bench で確認できるようにする (Refs #50)。
        if now.saturating_sub(last_batt) >= BATT_INTERVAL_MS {
            match alc_hub_board::power::read_status(&mut i2c) {
                Ok(ps) => {
                    println!(
                        "EVT BATT pct={} mv={} vbus={} chg={} bat={} adc={:02X} gauge={} vraw={:02X},{:02X} raw={:02X},{:02X}",
                        ps.battery_percent,
                        ps.battery_mv,
                        ps.vbus_present as u8,
                        ps.charge_state,
                        ps.battery_present as u8,
                        ps.adc_cfg,
                        ps.gauge_raw,
                        ps.volt_raw.0,
                        ps.volt_raw.1,
                        ps.status_raw.0,
                        ps.status_raw.1,
                    );
                    if let Ok(mut st) = status.lock() {
                        st.power_read = true;
                        st.battery_present = ps.battery_present;
                        st.battery_percent = ps.battery_percent;
                        st.battery_mv = ps.battery_mv;
                        st.vbus_present = ps.vbus_present;
                        st.charge_state = ps.charge_state;
                    }
                }
                Err(e) => log::warn!("ui: バッテリー状態取得に失敗: {e:?}"),
            }
            last_batt = now;
        }

        // --- コマンド (ホスト / BLE) ---
        while let Ok(cmd) = rx.try_recv() {
            last_activity = now;
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
                        items,
                        temp,
                        bp,
                        alcohol,
                        done_at,
                        ..
                    } = &mut screen
                    {
                        *temp = Some(celsius);
                        mark_done_if_complete(*items, temp, bp, alcohol, done_at, now);
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
                // 血圧: 点呼の構成に含むときだけ画面に反映する。OFF (保留) なら
                // 点呼中も待機中も画面は動かさない (測定値はログ・WS には残る)
                UiCommand::BloodPressure {
                    systolic,
                    diastolic,
                    pulse,
                } => {
                    if let Screen::Measuring {
                        items,
                        temp,
                        bp,
                        alcohol,
                        done_at,
                        ..
                    } = &mut screen
                    {
                        if items.bp {
                            *bp = Some((systolic, diastolic, pulse));
                            mark_done_if_complete(*items, temp, bp, alcohol, done_at, now);
                            dirty = true;
                        } else {
                            log::info!("ui: 血圧は点呼の構成外 (TENKO BP OFF) — 画面には出さない");
                        }
                        if acquiring == Some(DeviceKind::BloodPressure) {
                            acquiring = None;
                        }
                    } else if !current_items(&status).bp {
                        log::info!("ui: 血圧表示を抑制 (TENKO BP OFF)");
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
                    if let Screen::Measuring {
                        items,
                        temp,
                        bp,
                        alcohol,
                        alc_stage,
                        done_at,
                    } = &mut screen
                    {
                        *alcohol = Some((ok, value));
                        *alc_stage = None;
                        mark_done_if_complete(*items, temp, bp, alcohol, done_at, now);
                        dirty = true;
                    } else {
                        screen = Screen::Result { ok, value };
                        entered = now;
                        dirty = true;
                    }
                }
                // FC-1200 の進行状態: 点呼画面のアルコール欄のみ更新。
                // 他画面では無視 (測定フローは FC-1200 側が勝手に進むため)
                UiCommand::AlcoholStage(stage) => {
                    if let Screen::Measuring { alc_stage, .. } = &mut screen {
                        if *alc_stage != stage {
                            *alc_stage = stage;
                            dirty = true;
                        }
                    }
                }
                // 免許証タップ: ログ確認を LOG_LOCK_MS 封じ、待機系の画面なら
                // 点呼確認画面へ直行する。点呼中 (Measuring) と QR 表示中は
                // 奪わない (点呼中のタップは免許確認として既にログに残っている)
                UiCommand::License(card) => {
                    log_lock.arm(now);
                    if license_prompt_allowed(&screen) {
                        let expiry =
                            tenko_prompt::expiry_state(&card.expiry, today_yyyymmdd().as_deref());
                        if expiry == ExpiryState::Expired {
                            println!("EVT LICENSE_EXPIRED {}", card.expiry);
                        }
                        screen = Screen::Confirm { card, expiry };
                        entered = now;
                        dirty = true;
                    } else {
                        log::info!("ui: 免許証の点呼確認を抑制 (操作中の画面を優先)");
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
                        UiCommand::Measure => new_tenko(current_items(&status)),
                        UiCommand::Error { message } => Screen::Error { message },
                        UiCommand::Reset => Screen::Idle,
                        UiCommand::Rotate(_)
                        | UiCommand::Temperature { .. }
                        | UiCommand::BloodPressure { .. }
                        | UiCommand::BleAcquiring { .. }
                        | UiCommand::BleIdle
                        | UiCommand::AlcoholStage(_)
                        | UiCommand::License(_)
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
            // 点呼: 必須項目が揃ったら 5 秒表示して待機画面へ
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
            // 点呼確認: かざしただけで立ち去ったら待機画面へ
            Screen::Confirm { .. } if elapsed > tenko_prompt::CONFIRM_TIMEOUT_MS => {
                println!("EVT CONFIRM_TIMEOUT");
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
            last_activity = now;
        } else if let Some(p) = last_touch.take() {
            let (_, y) = map_touch(i32::from(p.x), i32::from(p.y), rotation, LCD_W, LCD_H);
            let logical_h = if rotation == 90 || rotation == 270 {
                LCD_W
            } else {
                LCD_H
            };
            if let Some(next) = on_click(
                &screen,
                y,
                logical_h,
                log_lock.is_locked(now),
                current_items(&status),
            ) {
                // 免許証から始めた点呼: 札を控えておき、session_id 発番時に送る
                if let (Screen::Confirm { card, .. }, Screen::Measuring { .. }) = (&screen, &next) {
                    pending_license = Some(card.clone());
                }
                screen = next;
                entered = now;
                dirty = true;
            }
        }

        // --- バックライト減光 (画面焼け対策): 無操作が続いたら最低輝度、
        // 操作があれば即復帰。set_backlight の呼び出しは減光/復帰の
        // 境界を跨いだ瞬間だけ (I2C を毎ループ叩かない)
        let idle_ms = now.saturating_sub(last_activity);
        if !backlight_dimmed && idle_ms >= config::BACKLIGHT_IDLE_DIM_MS {
            match alc_hub_board::power::set_backlight(&mut i2c, 0) {
                Ok(()) => {
                    backlight_dimmed = true;
                    println!("EVT BACKLIGHT_DIM");
                }
                Err(e) => log::warn!("ui: バックライト減光失敗: {e:?}"),
            }
        } else if backlight_dimmed && idle_ms < config::BACKLIGHT_IDLE_DIM_MS {
            match alc_hub_board::power::set_backlight(&mut i2c, 100) {
                Ok(()) => {
                    backlight_dimmed = false;
                    println!("EVT BACKLIGHT_ON");
                }
                Err(e) => log::warn!("ui: バックライト復帰失敗: {e:?}"),
            }
        }

        // --- 描画 ---
        let lock_secs = log_lock.remaining_secs(now);
        if dirty {
            let st = status.lock().map(|s| s.clone()).unwrap_or_default();
            screens::draw_full(&mut display, &screen, &st, now, entered, lock_secs);
            last_bar = now;
            last_spin = now;
            last_lock_secs = lock_secs;
            dirty = false;
        } else {
            // ログ確認ロックの残り秒数: 変化した秒だけ画面ごとの定位置を部分更新
            // (メニュー下段 / 待機の最下行 / 点呼確認のキャンセル側。全面クリア
            // しないので他の要素は blink しない)
            if lock_secs != last_lock_secs {
                screens::draw_lock_countdown(&mut display, &screen, lock_secs);
                last_lock_secs = lock_secs;
            }
            if now.saturating_sub(last_bar) >= 1000 {
                let st = status.lock().map(|s| s.clone()).unwrap_or_default();
                // 時計・インジケータのみの部分更新 (全面クリアしない — blink 防止)
                screens::update_status_bar(&mut display, &st, now);
                if let Screen::Qr { timeout_ms, .. } = &screen {
                    let remain_s = timeout_ms.saturating_sub(now.saturating_sub(entered)) / 1000;
                    screens::draw_qr_countdown(&mut display, remain_s);
                }
                // 点呼確認: 「点呼を開始」の下の残り秒数を毎秒更新
                if let Screen::Confirm { .. } = &screen {
                    screens::draw_confirm_countdown(
                        &mut display,
                        tenko_prompt::confirm_remaining_secs(entered, now),
                    );
                }
                last_bar = now;
            }
            // 点呼画面: BLE 取得中の機器ラベル横スピナーをアニメーション
            // (未取得の項目のみ — tenko_spinner_visible 参照)
            if let Some(kind) = acquiring {
                if let Screen::Measuring { items, .. } = &screen {
                    if tenko_spinner_visible(&screen, kind)
                        && now.saturating_sub(last_spin) >= 150
                    {
                        spin_phase = (spin_phase + 1) % 8;
                        screens::draw_tenko_spinner(&mut display, *items, kind, spin_phase);
                        last_spin = now;
                    }
                }
            }
        }

        // --- 点呼セッションの同期 (Refs #112) ---
        // 遷移箇所ごとに書くのではなく「今 Measuring にいるか」を毎周見て差分で
        // 更新する。Measuring への入口 (MEASURE コマンド / メニューのタップ) も
        // 出口 (RESET / 自動クローズ / タイムアウト / タッチ) も複数あり、
        // 個別に捕まえると必ずどこかで取りこぼすため。
        let now_in_session = matches!(screen, Screen::Measuring { .. });
        if now_in_session != in_session {
            in_session = now_in_session;
            let session_id = if now_in_session {
                let id = session_gen.next();
                println!("EVT TENKO_SESSION {id}");
                Some(id)
            } else {
                None
            };
            if let Ok(mut st) = status.lock() {
                st.session_id = session_id;
            }
            // session_id を status に載せた後で送る (recorder はそれを読んで付ける)。
            // メニューから始めた点呼では札が無いので何も送らない
            if let Some(card) = pending_license.take() {
                if now_in_session {
                    if let (Ok(issue), Ok(expiry)) = (card.issue.parse::<u32>(), card.expiry.parse::<u32>()) {
                        let _ = meas_tx.send(Measurement::License {
                            issue,
                            expiry,
                            at_ms: now,
                        });
                    }
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
        Screen::Measuring {
            items, temp, bp, ..
        } => match kind {
            DeviceKind::Thermometer => temp.is_none(),
            // 血圧 OFF なら段が無いので回さない
            DeviceKind::BloodPressure => items.bp && bp.is_none(),
        },
        _ => false,
    }
}

/// 現在の点呼構成 (HubStatus 経由。`TENKO BP` で変わる)
fn current_items(status: &SharedStatus) -> TenkoItems {
    TenkoItems {
        bp: status.lock().map(|s| s.tenko_bp).unwrap_or(false),
    }
}

/// 点呼画面の初期状態
fn new_tenko(items: TenkoItems) -> Screen {
    Screen::Measuring {
        items,
        temp: None,
        bp: None,
        alcohol: None,
        alc_stage: None,
        done_at: None,
    }
}

/// 必須項目が揃った瞬間を記録する (揃った後の測り直しでは時刻を動かさない)
fn mark_done_if_complete(
    items: TenkoItems,
    temp: &Option<f32>,
    bp: &Option<(f32, f32, Option<f32>)>,
    alcohol: &Option<(bool, String)>,
    done_at: &mut Option<u64>,
    now: u64,
) {
    if done_at.is_none() && items.complete(temp.is_some(), bp.is_some(), alcohol.is_some()) {
        *done_at = Some(now);
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

/// 免許証タップで点呼確認画面へ遷移してよい画面か。
///
/// - 待機 / メニュー / ログ / バイタル表示 / 結果 / エラー: 遷移する
///   (ログ確認中でも免許証が来たら点呼確認が優先 — かざした本人が目の前にいる)
/// - 確認画面: 再タップで情報を更新 (ロックも延長)
/// - 点呼中 (Measuring) / QR: 奪わない (進行中の点呼・ホスト主導の操作を守る)
fn license_prompt_allowed(screen: &Screen) -> bool {
    !matches!(screen, Screen::Measuring { .. } | Screen::Qr { .. })
}

/// 今日の日付 "YYYYMMDD" (JST)。NTP 未同期なら None (期限判定は Unknown になる)
fn today_yyyymmdd() -> Option<String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| alc_hub_core::clock::jst_yyyymmdd(d.as_secs() as i64))
}

/// 点呼開始: ホストへ通知し、測定待ち画面を作る (構成は現在の設定で固定)
fn start_tenko(items: TenkoItems) -> Screen {
    println!("EVT TENKO_START");
    new_tenko(items)
}

/// タップ時の画面遷移先 (None = 変化なし)。y は回転補正済みの論理座標。
/// `log_locked` は免許証タップ後のログ確認ロック中か (メニュー下段を無効化)。
/// `items` は点呼を始める場合の構成
fn on_click(
    screen: &Screen,
    y: i32,
    logical_h: i32,
    log_locked: bool,
    items: TenkoItems,
) -> Option<Screen> {
    match screen {
        Screen::Idle => Some(Screen::Menu),
        // ヒット判定は描画 (screens::draw_menu) と同じ幾何 (バー直下から 2 等分)
        Screen::Menu => match tenko_prompt::menu_hit(y, screens::BAR_H, logical_h, log_locked)? {
            MenuChoice::Tenko => Some(start_tenko(items)),
            MenuChoice::Log => Some(Screen::Log),
        },
        Screen::Confirm { .. } => {
            match tenko_prompt::confirm_hit(y, screens::CONFIRM_BUTTONS_TOP, logical_h)? {
                ConfirmChoice::Start => Some(start_tenko(items)),
                ConfirmChoice::Cancel => {
                    println!("EVT TENKO_CANCEL");
                    Some(Screen::Idle)
                }
            }
        }
        Screen::Log
        | Screen::Measuring { .. }
        | Screen::Result { .. }
        | Screen::Error { .. }
        | Screen::Temperature { .. }
        | Screen::BloodPressure { .. } => Some(Screen::Idle),
        // QR は誤タップで閉じない (ホストの RESET / タイムアウトのみ)
        Screen::Qr { .. } => None,
    }
}
