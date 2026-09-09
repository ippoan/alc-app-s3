//! alc-hub-atoms3-alarm: 点呼端末の警告デバイス (ippoan/alc-app-s3#135)。
//!
//! 点呼キオスク (ブラウザ) の異常を人に気付かせる据置ブザー。キオスクが USB CDC で
//! 3 秒ごとに「正常」(`HB OK`) を送り、**途切れたら端末が自分の判断で鳴る**。
//!
//! **ブラウザが「鳴れ」と命令する形にしない**のが設計の要 — 命令駆動だと
//! ブラウザ / PC が落ちたときに命令が来ず沈黙する = **一番危ないケースで
//! 鳴らない**。沈黙を異常とみなせば、ブラウザのクラッシュ・タブを閉じた・
//! PC のフリーズを**同じ形で**拾える (plan/standing-devices.md §4.1)。
//!
//! # スコープ — ネットワークなし・USB だけで完結する版
//!
//! plan §6 のマイルストーン (2)-2。**Wi-Fi / WS 常時接続 (案 A) と音声メッセージは
//! 入れていない**。点呼の呼び出しは heartbeat 相乗り (`HB OK call=1`、案 B) で受ける。
//! したがって本 crate は `alc-hub-drivers` の `speaker` feature だけを使い、
//! NFC / LAN / WS uplink / OTA / Wi-Fi はどれも配線しない。
//!
//! **許容する穴**: USB 給電なので PC の電源が落ちるとブザーも死ぬ。
//! ユーザー判断で**許容** (人が見れば分かる異常のため)。独立給電にすると
//! 「PC が落ちた」と「USB が抜けた」の区別が付かなくなるので、
//! **穴を塞ぐより穴があることを運用に伝える方が安い** (plan §4.1)。
//!
//! # 判定はここに書かない
//!
//! 沈黙・NG・呼び出しの判定と状態機械は [`alc_hub_core::alarm`] (ホストで
//! テスト済みの純粋ロジック)。本ファイルがやるのは **[`Action`] を実行すること
//! だけ** — 音を鳴らす / ホストへ行を書き出す。閾値 (`SILENCE_MS` 等) を
//! ここに書き写さないこと: 2 か所に数値があると片方だけ直る。
//!
//! # ハード構成
//!
//! **Atom VoiceS3R** (M5Stack Atom EchoS3R, SKU C126-ECHO /
//! ESP32-S3-PICO-1-N8R8: 8MB Flash + 8MB Octal PSRAM) を USB-C でキオスク PC へ。
//! `crates/atoms3-timecard` と**同じ本番機**で、使うのは音とボタンだけ。
//!
//! - **内蔵オーディオ** (ES8311 + NS4150B): I2C SDA=G45 / SCL=G0、
//!   I2S BCLK=G17 / WS=G3 / DOUT=G48、アンプ有効化 G18。**MCLK (G11) は配線しない**
//! - **本体ボタン G41** (active-low)。根拠は M5Unified の `_update_button_state`
//!   が AtomS3 / AtomS3 Lite / AtomS3U / AtomS3R / VoiceS3R を**どれも G41**と
//!   明示していること (plan §4.4)。LED は無い (#151) ので、**この機の反応は音だけ**
//!
//! # 起動順 (変えてはいけない)
//!
//! `crashlog::init` → `Settings::new` → `heap::start` → `console::start` → 音の初期化。
//! **`crashlog::init` は `heap::start` より前** (配線漏れで `.noinit` のゴミ帳簿に
//! 書いて boot loop になった実害が 2026-07-14 にある)。
//!
//! 音の初期化は**さらに内部の順番が命** (Refs #102):
//! `Speaker::new` → `feed_silence` → `es8311::init_amp` → `start_player`。
//! ESP-IDF 新 I2S ドライバは FIFO 空で BCK を止めるため、クロックを流す前に
//! コーデックを起こすと PLL がロックせず**完全に無音**になる。
//!
//! # 鳴動ループは main タスク自身
//!
//! 50ms ごとにボタンを見て [`AlarmMonitor::tick`] を回すのは main の末尾ループ。
//! **専用スレッドを立てていない** — アンプ有効化ピン (`_amp_en`) は drop すると
//! 出力が落ちるので main が持ち続ける必要があり、ボタンの `PinDriver` も
//! ここにあるのが自然。[`AlarmMonitor`] は `Arc<Mutex<_>>` でコンソール
//! スレッド (heartbeat の受け手) と共有する。

mod console;

use alc_hub_common::{
    config,
    settings::Settings,
    status::{now_ms, HubStatus, SharedStatus},
};
use alc_hub_core::alarm::{Action, AlarmMonitor};
use alc_hub_drivers::speaker::Sound;
use alc_hub_drivers::{crashlog, es8311, heap, speaker};
use anyhow::Result;
use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{PinDriver, Pull},
    i2c::{config::Config as I2cConfig, I2cDriver},
    peripherals::Peripherals,
    units::Hertz,
};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use std::sync::{mpsc, Arc, Mutex};

/// 鳴動ループの周期。`ALERT_PERIOD_MS` (1800) に対して十分細かく、
/// ボタンのデバウンス 3 サンプル = 150ms が体感で遅れない値
const TICK_MS: u32 = 50;

/// ボタンの読みが何サンプル続いたら確定とみなすか (50ms × 3 = 150ms)
const BUTTON_DEBOUNCE_SAMPLES: u8 = 3;

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    // 前回リセットの解析 + ログ捕捉 hook (`EVT BOOT reset=…` を出す)。
    // heap.rs の note() がリングに書くため、heap::start より前に必ず呼ぶこと。
    // **crash の中身は送らない** — 本機は uplink を持たないので、
    // 拾った panic 前ログは `LOG DUMP` で現地から読む
    let _ = crashlog::init();
    log::info!(
        "alc-hub-atoms3-alarm v{} 起動",
        config::firmware_version_full()
    );

    let p = Peripherals::take()?;

    // NVS。本機に登録するものは無いが、共通コンソール (AUTH / WS 等) が
    // Settings を要求するため用意する
    let nvs_partition = EspDefaultNvsPartition::take()?;
    let settings = Settings::new(nvs_partition)?;

    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));
    // ヒープ監視 (OOM 捕捉 + low-water 計測) は重いアロケーションより先に登録
    heap::start(Arc::clone(&status))?;

    // 鳴動判定。コンソールスレッド (heartbeat の受け手) と共有する
    let monitor = Arc::new(Mutex::new(AlarmMonitor::new()));

    // ホストコンソール (HB / STATUS / PING / HEAP / LOG)
    console::start(Arc::clone(&monitor), Arc::clone(&status), settings.clone())?;

    // 内蔵オーディオ (ES8311 + NS4150B)。I2C は内蔵バス (SDA=G45 / SCL=G0)
    let mut audio_i2c = I2cDriver::new(
        p.i2c1,
        p.pins.gpio45,
        p.pins.gpio0,
        &I2cConfig::new().baudrate(Hertz(400_000)),
    )?;
    // 内蔵バスに誰が居るか。ES8311 (0x18) が見えなければ配線かポートが違う
    es8311::probe_bus(&mut audio_i2c);

    // **順番が命** (Refs #102): I2S を立てて BCK/WS を実際に流してから
    // コーデックを起こす。`tx_enable()` だけではクロックが出ず PLL がロックしない。
    // MCLK (G11) は配線しない — ES8311 側を MCLK=BCLK で使う (es8311.rs)。
    //
    // ★ 音は**失敗しても致命にしない**が、**この機は音しか持たない**ので
    //   失敗はそのまま「警告デバイスとして役に立たない」を意味する。
    //   `EVT SPEAKER_NG` を出してキオスク側から見えるようにする。
    //   `_amp_en` は NS4150B の有効化ピン (G18) で、**drop すると出力が落ちる**ので
    //   main が持ち続ける
    let (speaker_tx, _amp_en) = match (|| -> Result<_> {
        let mut spk = speaker::Speaker::new(
            p.i2s1,
            p.pins.gpio17.into(), // BCLK
            p.pins.gpio3.into(),  // WS (LRCK)
            p.pins.gpio48.into(), // DOUT
        )?;
        spk.feed_silence(300)?;
        let en = es8311::init_amp(&mut audio_i2c, p.pins.gpio18.into())?;
        // 無音だったときの切り分け用に初期化直後の全レジスタを残す (Refs #102)
        es8311::dump_regs(&mut audio_i2c);
        Ok((speaker::start_player(spk)?, en))
    })() {
        Ok((tx, en)) => (Some(tx), Some(en)),
        Err(e) => {
            log::warn!("speaker: 初期化失敗 — 鳴らせない状態で継続する: {e:#}");
            println!("EVT SPEAKER_NG");
            (None, None)
        }
    };

    // 本体ボタン G41 (active-low)。押したら鳴動を黙らせる (音は鳴らさない)。
    // 内部プルアップは `input` の第 2 引数で入れる (esp-idf-hal 0.46 では
    // `set_pull` が private で後から付け替えられない)
    let button = PinDriver::input(p.pins.gpio41, Pull::Up)?;

    // 鳴動ループ。判定は monitor が持ち、ここは Action の実行だけ
    let mut raw_pressed = false;
    let mut streak: u8 = 0;
    let mut pressed = false;
    loop {
        FreeRtos::delay_ms(TICK_MS);
        let now = now_ms();

        // デバウンス: 同じ読みが BUTTON_DEBOUNCE_SAMPLES 続いたときだけ確定を動かす。
        // 離すときも同じ回数を要求する (バウンドで押下エッジが二重に立たない)
        let level = button.is_low(); // active-low: low = 押している
        if level == raw_pressed {
            streak = streak.saturating_add(1);
        } else {
            raw_pressed = level;
            streak = 1;
        }
        let mut button_edge = false;
        if streak >= BUTTON_DEBOUNCE_SAMPLES && pressed != raw_pressed {
            pressed = raw_pressed;
            // 押下エッジだけを monitor へ渡す (離したときは何もしない)
            button_edge = pressed;
        }

        let mut actions = Vec::new();

        // lock 失敗 (コンソールスレッドが panic した) なら鳴動判定は続けられない。
        // ログだけ残してループは回し続ける (再起動は人の判断に委ねる)
        match monitor.lock() {
            Ok(mut m) => {
                if button_edge {
                    actions.extend(m.on_button(now));
                }
                actions.extend(m.tick(now));
            }
            Err(e) => log::error!("alarm: monitor の lock に失敗: {e}"),
        }

        for action in actions {
            match action {
                // 鳴らし直しの周期は monitor が刻む (alarm::ALERT_PERIOD_MS)。
                // **再生スレッド側でループを作らない** — 占有するとボタンで
                // 止めたのに鳴り続ける (speaker.rs の Sound::Alert の doc)
                Action::PlayAlert => send(&speaker_tx, Sound::Alert),
                Action::PlayResolved => send(&speaker_tx, Sound::AlertResolved),
                // 黙らせているあいだの短い合図 (alarm::MUTED_TICK_MS ごと)。
                // 完全な無音だと異常が続いていることを忘れられる
                Action::PlayMutedTick => send(&speaker_tx, Sound::MutedTick),
                // 沈黙 (繋がっていない) のときの短い 2 連 (alarm::SILENCE_TICK_MS ごと)
                Action::PlaySilenceTick => send(&speaker_tx, Sound::SilenceTick),
                // キオスクのバナー用。遷移のたび + BANNER_MS ごとに出る
                Action::Emit(line) => println!("{line}"),
            }
        }
    }
}

/// 再生依頼をキューへ積む。**ここで待たない** — I2S の write はブロッキングで、
/// 直接鳴らすと鳴動ループが 1 秒近く止まりボタンの反映が遅れる
fn send(speaker: &Option<mpsc::Sender<Sound>>, sound: Sound) {
    if let Some(tx) = speaker {
        let _ = tx.send(sound);
    }
}
