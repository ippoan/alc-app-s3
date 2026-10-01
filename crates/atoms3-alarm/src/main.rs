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
//! したがって本 crate は `alc-hub-drivers` の `speaker` と `nfc` feature だけを使い、
//! LAN / WS uplink / OTA / Wi-Fi はどれも配線しない。
//!
//! # Unit NFC — 付いていれば読む (Refs ippoan/alc-app#387)
//!
//! 運行管理者が席でカードをかざすと、席のブラウザがその人を「この席の運行管理者」に
//! 登録する。本機の役目は**読んだカードを USB の 1 行でブラウザへ渡すこと**だけ:
//!
//! - IC カード (FeliCa IDm / NFC-A UID) → `EVT NFC_LOGIN card_id=… card_kind=…`
//!   ([`on_card`]。行の形は [`alc_hub_core::nfc_login`])
//! - 従来 IC 運転免許証 → 読み取りの正本 (`alc_hub_drivers::nfc`) が出す
//!   `EVT NFC_LICENSE issue=… expiry=…` をそのまま使う。**ここでは何も出さない**
//!
//! **打刻ではない。** 本機は uplink を持たないので、サーバへは何も送らない
//! (`EVT TIMECARD` も出さない — alc-app はそれを打刻として拾う)。
//!
//! **Unit NFC は任意の部品で、無い機体にも同じファームを焼く。** 無ければ
//! 読み取りスレッドが 5 秒おきに初期化を試し続けるだけで (`EVT NFC_INIT_NG` は
//! rc が変わったときだけ)、警告の本務 (heartbeat の監視・鳴動・ボタン) は変わらない。
//! flavor も `DEVICE` の名乗りも分けない。あとから挿せば、再起動なしで
//! 次の再試行から読み始める。
//!
//! **許容する穴**: USB 給電なので PC の電源が落ちるとブザーも死ぬ。
//! ユーザー判断で**許容** (人が見れば分かる異常のため)。独立給電にすると
//! 「PC が落ちた」と「USB が抜けた」の区別が付かなくなるので、
//! **穴を塞ぐより穴があることを運用に伝える方が安い** (plan §4.1)。
//!
//! # 判定はここに書かない
//!
//! 沈黙・NG・呼び出しの判定と状態機械は [`alc_hub_core::alarm`] (ホストで
//! テスト済みの純粋ロジック)。閾値 (`SILENCE_MS` 等) をここに書き写さないこと:
//! 2 か所に数値があると片方だけ直る。
//!
//! `Action` の実行 (音を鳴らす / ホストへ行を書き出す) と、判定器を lock して
//! heartbeat を渡す手続きも **[`alc_hub_drivers::alarm`] の共通実装**を通る —
//! CoreS3 (root の `alc-hub-cores3`) も同じ関数を使うため、ここに書き写すと
//! 実機の鳴り方が機種で割れる (issue #187)。
//!
//! # ハード構成
//!
//! **Atom VoiceS3R** (M5Stack Atom EchoS3R, SKU C126-ECHO /
//! ESP32-S3-PICO-1-N8R8: 8MB Flash + 8MB Octal PSRAM) を USB-C でキオスク PC へ。
//! `crates/atoms3-timecard` と**同じ本番機**で、使うのは音とボタンと Unit NFC (任意)。
//!
//! - **内蔵オーディオ** (ES8311 + NS4150B): I2C SDA=G45 / SCL=G0、
//!   I2S BCLK=G17 / WS=G3 / DOUT=G48、アンプ有効化 G18。**MCLK (G11) は配線しない**
//! - **本体ボタン G41** (active-low)。根拠は M5Unified の `_update_button_state`
//!   が AtomS3 / AtomS3 Lite / AtomS3U / AtomS3R / VoiceS3R を**どれも G41**と
//!   明示していること (plan §4.4)。LED は無い (#151) ので、**この機の反応は音だけ**
//! - **Unit NFC** (ST25R3916、任意): Grove SDA=G2 / SCL=G1。I2C は **I2C_NUM_0** で、
//!   バスは C++ 側 (nfc_shim) が立てる。**Rust 側で `p.i2c0` の `I2cDriver` を
//!   作らないこと** (二重 install で abort する)。音の I2C は別ポートの i2c1
//!
//! # 起動順 (変えてはいけない)
//!
//! `crashlog::init` → `Settings::new` → `heap::start` → `console::start` → 音の初期化
//! → `nfc::start`。
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
//!
//! NFC の読み取りは別スレッド (`nfc::start` が立てる) で、このループには何も足さない。
//! ただし**ループに入る前に main タスクの優先度を上げる** — 理由は [`MAIN_TASK_PRIO`]。

mod console;

use alc_hub_common::{
    config,
    settings::Settings,
    status::{now_ms, HubStatus, SharedStatus},
};
use alc_hub_core::alarm::AlarmMonitor;
use alc_hub_drivers::nfc::NfcEvent;
use alc_hub_drivers::timecard::Punch;
use alc_hub_drivers::{alarm, crashlog, es8311, heap, nfc, speaker};
use anyhow::Result;
use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{PinDriver, Pull},
    i2c::{config::Config as I2cConfig, I2cDriver},
    peripherals::Peripherals,
    units::Hertz,
};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use std::sync::{Arc, Mutex};

/// 鳴動ループの周期。`ALERT_PERIOD_MS` (1800) に対して十分細かく、
/// ボタンのデバウンス 3 サンプル = 150ms が体感で遅れない値
const TICK_MS: u32 = 50;

/// ボタンの読みが何サンプル続いたら確定とみなすか (50ms × 3 = 150ms)
const BUTTON_DEBOUNCE_SAMPLES: u8 = 3;

/// nfc_shim (C++ 側) に立てさせる I2C ポート。音の内蔵バスは i2c1 を使うので、
/// Grove の Unit NFC は I2C_NUM_0 (atoms3-timecard と同値)
const I2C_PORT_NFC: i32 = 0;

/// 鳴動ループ (main タスク) の優先度。**NFC スレッドより 1 つ上**に置く。
///
/// main タスクは既定で優先度 1・CPU0 固定 (`ESP_TASK_MAIN_PRIO` /
/// `CONFIG_ESP_MAIN_TASK_AFFINITY_CPU0`)。NFC の読み取りスレッドは pthread 既定の
/// 優先度 (`CONFIG_PTHREAD_TASK_PRIO_DEFAULT`) でコア固定なしで立ち、待機中も毎周
/// カードを探す (`PresenceGate::AlwaysPoll`)。その 1 周は ST25R3916 の detect の
/// ビジーウェイトで数百 ms CPU を離さない (`alc_hub_drivers::nfc` の実測)。
/// **既定のままだと、NFC スレッドが CPU0 に載った間、優先度 1 の鳴動ループは
/// 周の切れ目でしか走れず、ボタンと鳴動の判定が数百 ms 単位で遅れる。**
///
/// 優先度を上にしておけば、50ms の tick が来るたびに FreeRTOS が NFC スレッドを
/// 横取りしてループを 1 周させる。ループの 1 周はピンの読みと判定器の lock だけで
/// すぐ寝るので、NFC の読み取りを止める時間は無視できる。
///
/// コンソール (heartbeat の受け手) と再生スレッドは NFC スレッドと同じ優先度・
/// コア固定なしなので、2 コアと時分割で飢えない (こちらは既定のまま)。
///
/// **上げた main がそれらを飢えさせることも無い** — ループは毎周
/// `FreeRtos::delay_ms(TICK_MS)` で必ず CPU を明け渡し、1 周の仕事はピンの読み・
/// 判定器の lock・再生キューへの投入 (`alarm::run_actions` は待たない) と、
/// 状態が変わったとき / 5 秒ごとの `EVT ALARM` 1 行だけ。**ここに待つ処理や
/// 重い処理を足さないこと** (足すと今度は main が他を止める)。
/// Unit NFC が無い機体でも同じ優先度で回す (NFC スレッドは 5 秒おきに起きるだけで、
/// 上の事情は変わらない — 機体で分けない)
const MAIN_TASK_PRIO: u32 = esp_idf_svc::sys::CONFIG_PTHREAD_TASK_PRIO_DEFAULT + 1;

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    // 前回リセットの解析 + ログ捕捉 hook (`EVT BOOT reset=…` を出す)。
    // heap.rs の note() がリングに書くため、heap::start より前に必ず呼ぶこと。
    // **crash の中身は送らない** — 本機は uplink を持たないので、
    // 拾った panic 前ログは `LOG DUMP` で現地から読む
    let (reset_code, _) = crashlog::init();
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

    // 鳴動判定。コンソールスレッド (heartbeat の受け手) と共有する。
    // USB/JTAG 起因の reset で前回稼働が武装済み (`.noinit` のフラグ) なら
    // 武装済みで生成する (CoreS3 と同じ分岐、issue #194)。それ以外は従来どおり
    // 起動猶予つき (BOOT_GRACE_MS)
    let monitor = Arc::new(Mutex::new(
        if alc_hub_core::crashlog::is_usb_serial_reset(reset_code)
            && alarm::restore_armed_flag()
        {
            alc_hub_common::evtlog::emit(&format!("EVT ALARM_RESTORED reset={reset_code}"));
            AlarmMonitor::with_boot_grace(Some(alc_hub_core::alarm::SILENCE_MS))
        } else {
            AlarmMonitor::new()
        },
    ));

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
            alc_hub_common::evtlog::emit("EVT SPEAKER_NG");
            (None, None)
        }
    };

    // Unit NFC (ST25R3916、任意): Grove (SDA=G2 / SCL=G1)。読み取りループは
    // hub-drivers/src/nfc.rs (CoreS3 / atoms3-timecard と共有)。**ここに NFC の
    // コードを書かない。** 並びと常時ポーリングは atoms3-timecard と同じ
    // (理由は nfc::PollOrder / nfc::PresenceGate の doc)。
    //
    // ★ **失敗しても main を落とさない** — 警告デバイスの本務は鳴ること。
    //   ここで返る Err はスレッドを立てられなかったときだけで、Unit NFC が
    //   付いていない機体は Ok のまま、スレッドが 5 秒おきに初期化を試し続ける
    if let Err(e) = nfc::start(
        I2C_PORT_NFC,
        p.pins.gpio2.into(),
        p.pins.gpio1.into(),
        nfc::PollOrder::LicenseFirst,
        nfc::PresenceGate::AlwaysPoll,
        Arc::clone(&status),
        on_card,
    ) {
        log::warn!("nfc: 読み取りスレッドを起動できない — カードを読まずに継続する: {e:#}");
        alc_hub_common::evtlog::emit("EVT NFC_START_NG");
    }

    // 本体ボタン G41 (active-low)。押したら鳴動を黙らせる (音は鳴らさない)。
    // 内部プルアップは `input` の第 2 引数で入れる (esp-idf-hal 0.46 では
    // `set_pull` が private で後から付け替えられない)
    let button = PinDriver::input(p.pins.gpio41, Pull::Up)?;

    // 鳴動ループを NFC スレッドより上の優先度で回す (理由は MAIN_TASK_PRIO)。
    // null = 呼び出したタスク自身 (= main)
    unsafe { esp_idf_svc::sys::vTaskPrioritySet(core::ptr::null_mut(), MAIN_TASK_PRIO) };

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

        // 音とホストへの行は共通実装へ (alc_hub_drivers::alarm)。CoreS3 も
        // 同じ関数を通る — **本機だけ直しても実機の鳴り方が割れる**。
        // 本機は `EVT ALARM` を出す (キオスクのバナー用) ので emit_lines = true
        alarm::run_actions(actions, &speaker_tx, true);
    }
}

/// カードを 1 枚読めたときの処理: IC カードなら `EVT NFC_LOGIN` を 1 行出す。
///
/// `NfcEvent` → (生の id, 種別) の写像は打刻と同じ `Punch::from_event`
/// (CoreS3 / atoms3-timecard と共有) を通す — **「どのイベントを人のカードと
/// みなすか」をここに書き写さない**。行にするかどうかと行の形は
/// [`alc_hub_core::nfc_login::evt_line`] (免許証は行にしない — 正本が
/// `EVT NFC_LICENSE` を出している)。
///
/// **打刻にしない**: 送信キューも uplink も持たないので、出口はこの println だけ。
/// 2 枚検知・読み取り失敗・電子車検証は何もしない (音も鳴らさない — 本機の音は警告)。
///
/// **`evtlog::emit` にしない** — `card_id` は人を特定できる値で、リングに残さない
/// (`alc_hub_common::evtlog` の「どの行を emit にするか」)
fn on_card(event: &NfcEvent) {
    let Some(punch) = Punch::from_event(event) else {
        return;
    };
    if let Some(line) = alc_hub_core::nfc_login::evt_line(&punch.card_id, punch.kind) {
        println!("{line}");
    }
}
