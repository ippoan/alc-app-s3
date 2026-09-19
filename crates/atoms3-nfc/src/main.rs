//! alc-hub-atoms3-nfc: AtomS3 Lite + Unit NFC (ST25R3916) ベンチ検証機。
//!
//! CoreS3 統合ハブから NFC 検証だけを切り出した独立ファームウェア
//! (issue #84 / plan/nfc-card-identity.md)。CoreS3 側は LAN/RS232 モジュール
//! 併用時、内蔵スピーカー(I2S DATA_OUT=固定 G13) と LAN CS ジャンパ
//! (G5=G1 / G15=G13、G1 は RS232M 自身の CS と衝突) が逃げ場なく競合するため
//! (plan/cores3-hub-consolidation.md 参照)、LAN/RS232 非搭載の AtomS3 Lite へ
//! NFC 検証を移設した。
//!
//! # 読み取りループは持たない (issue #146)
//!
//! **NFC の読み取り・重複抑止は `alc_hub_drivers::nfc` が正本。**本 crate は
//! そこへピンを渡し、通知 (LED) を受けるだけにする。以前はこのファイルが
//! 独自のポーリングループと「直前に読めた ID と違えば発火 / 読めなければ直前値を
//! クリア」の旧エッジ判定を持っていた。その判定こそ issue #103 で
//! 「1 タップ 2 重読み」の原因と特定したもので、
//! **検証機で測ると本番機 (crates/atoms3-timecard) と違う挙動が出る**
//! (issue #143 の実機計測がこれで成立しなかった)。検証機と本番機で
//! 重複抑止が同じ実装であることが、この crate の存在意義の前提になる。
//!
//! 通知は PC 側 `scripts/nfc_serial_beep.py` がシリアルログ (hub-drivers/nfc.rs
//! が出す `NFC IDm=…` / `免許証 交付 …`) を監視してビープを鳴らす方式に加え、
//! 本体 LED (WS2812) でもカード検知時に色を変える。待受中は暗い青 (生存確認)、
//! 検知成功 (IDm/免許証) は緑、読み取り失敗とカード 2 枚 (#143) は赤。
//! Atom VoiceS3R を測定台に使う build (`ATOMS3_NFC_VOICES3R`) は LED の代わりに
//! 内蔵スピーカーで、読めたときに短いビープを鳴らす (`VOICES3R` の doc)。
//!
//! **打刻は送らない。** WS/HTTP の uplink を持たないベンチ専用機なので、
//! ここでカードを読んでもサーバには何も届かない。
//!
//! # 血圧計用 PC の測定台 (`ATOMS3_NFC_VOICES3R` + `--features ble`、Refs ippoan/alc-app#353)
//!
//! Atom VoiceS3R の build は、現場の「血圧計をつないだ PC」に挿しっぱなしにする
//! **測定台**として配布する (Pages の `docs/atoms3-nfc.html`)。カードの読み取りに
//! 加えて、`OMRON BP ON` (NVS `omron_bp`、**既定 OFF**) のときだけ BLE central
//! (`alc_hub_ble`) を起こし、受けた測定値を CoreS3 / タイムカード端末と同じ
//! `recorder` 経由でホスト (PC のブラウザ) へ JSON で出す。
//!
//! **この機はネットワークを持たない**ので、上り (cf-alc-recorder) へは送らない —
//! サーバへ届けるのは PC 側の画面の仕事。`STATUS` / `OTA` も持たない
//! (ホストリンクが無いため) ので、コンソールは共通分だけを回す
//! `alc_hub_drivers::console::start_common` を呼ぶ。
//!
//! BLE は**既定の build には入らない** — AtomS3 Lite 向けの `sdkconfig.defaults`
//! は BT を持たず、esp32-nimble がリンクできない (実測: `esp_idf_sys::ble_*` が
//! 全滅する)。そのため依存は optional の `ble` feature に入れ、VoiceS3R の
//! overlay (`sdkconfig.voices3r.defaults`) とセットで有効にする。
//! **env と feature の食い違いは下の const アサーションがコンパイルエラーにする。**
//!
//! 配線: Grove Port A (SDA=G2 / SCL=G1)。nfc_shim 側が I2C バスを自前で
//! 立てるため、Rust 側で `Peripherals::take()` は LED (RMT + GPIO35) と
//! ピン番号の受け渡しにのみ使う。

// esp-idf-hal 0.46 の legacy RMT は deprecated 扱いだが、新 RMT API は ws2812 の
// ビット列を組む口が無く、また新旧が同一バイナリに載ると
// check_rmt_legacy_driver_conflict で abort する (sdkconfig.defaults 参照)
#![allow(deprecated)]

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use alc_hub_common::control::PairFlag;
use alc_hub_common::settings::Settings;
use alc_hub_common::status::{HubStatus, SharedStatus};
use alc_hub_drivers::nfc::{self, NfcEvent};
#[cfg(feature = "ble")]
use alc_hub_drivers::recorder;
use alc_hub_drivers::speaker::Sound;
use alc_hub_drivers::{console, es8311, speaker};
use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::i2c::{config::Config as I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::rmt::{
    config::TransmitConfig, FixedLengthSignal, PinState, Pulse, TxRmtDriver,
};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::nvs::EspDefaultNvsPartition;

/// nfc_shim (C++ 側) に立てさせる I2C ポート。本機は他に I2C を使わないので
/// I2C_NUM_0 (実機確認済み 2026-07-21)。CoreS3 は内部バスが I2C_NUM_0 を
/// 使うので向こうは 1。abort していた原因 ("CONFLICT! driver_ng is not allowed
/// to be used with this old driver") は sdkconfig.defaults の
/// CONFIG_I2C_SKIP_LEGACY_CONFLICT_CHECK=y で解消済み
const I2C_PORT_NFC: i32 = 0;

// デバッグのため一時的にかなり明るくして「見えているか」自体を確認する
// (元は暗め (0,0,8) だったが実機で無点灯と報告あり、2026-07-20)
const LED_IDLE: (u8, u8, u8) = (0, 0, 255);
const LED_OK: (u8, u8, u8) = (0, 255, 0);
const LED_ERR: (u8, u8, u8) = (255, 0, 0);

/// Atom VoiceS3R を測定台に使う build の印 (`ATOMS3_NFC_VOICES3R`、値は問わない)。
/// あちらは Octal PSRAM が GPIO35〜37 を内部で使い (uiflow-micropython の
/// M5STACK_Atom_EchoS3R が sdkconfig.spiram_oct)、WS2812 も載っていないので
/// LED を出さず、代わりに内蔵の ES8311 で読めたときに短いビープを鳴らす。
/// Grove は同じ SDA=G2 / SCL=G1 なので NFC 側は変えない。既定 (AtomS3 Lite) は従来どおり
const VOICES3R: bool = option_env!("ATOMS3_NFC_VOICES3R").is_some();

// ★ env (`ATOMS3_NFC_VOICES3R`) と feature (`ble`) は必ずセットで指定する。
// 片方だけだと「BLE をリンクするのに測定台の配線が無い」/「測定台なのに
// 血圧計が無い」が**黙って**できてしまい、実機に焼くまで気づけない。
// どちらも書き込みの現場でしか分からない種類の間違いなのでコンパイルで弾く
#[cfg(feature = "ble")]
const _: () = assert!(
    VOICES3R,
    "feature \"ble\" は ATOMS3_NFC_VOICES3R=1 の build 専用 (sdkconfig も overlay が要る)"
);
#[cfg(not(feature = "ble"))]
const _: () = assert!(
    !VOICES3R,
    "ATOMS3_NFC_VOICES3R=1 の build には --features ble が要る (血圧計が入らない)"
);

/// 検知色を維持する時間。RF リンクは per-exchange で確率的に落ちるため、
/// 成功直後の一時的な失敗で表示を戻すと「不安定」に見える (issue #96)
const LATCH: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("alc-hub-atoms3-nfc 起動 (Unit NFC 検証、Port A: SDA=G2/SCL=G1)");
    log::info!("firmware build time: {}", env!("FIRMWARE_BUILD_TIME"));

    let p = Peripherals::take()?;
    // AtomS3 Lite 本体 LED (WS2812)。GPIO38 という情報は Web 検索の要約のみで
    // 未検証だった — 無点灯の実機報告を受け M5Unified 公式ボード定義
    // (_pin_table_other0, "//RGBLED" コメント付き) を確認したところ実際は
    // GPIO35 だった (2026-07-20)。legacy RMT ドライバで直接ビットバンギング
    // (ws2812-esp32-rmt-driver crate は esp-idf-hal 0.46 と links 衝突するため不使用)
    // VoiceS3R の測定台では GPIO35 に出さない (VOICES3R の doc)
    let tx = if VOICES3R {
        None
    } else {
        Some(TxRmtDriver::new(
            p.rmt.channel0,
            p.pins.gpio35,
            &TransmitConfig::new().clock_divider(1),
        )?)
    };
    let led = Arc::new(Mutex::new(Led::new(tx)));

    // VoiceS3R の測定台だけ: 内蔵オーディオ (ES8311 + NS4150B)。つなぎは
    // atoms3-timecard と同じ形で、初期化と再生は hub-drivers の es8311 / speaker。
    // I2C は内蔵バス (SDA=G45 / SCL=G0) を i2c1 で — Grove の Unit NFC は
    // nfc_shim が I2C_NUM_0 を握るので取り合わない。
    // 音は失敗しても致命にしない (NFC の測定は続ける)。`amp_en` は NS4150B の
    // 有効化ピン (G18) で、drop すると出力が落ちるので main が持ち続ける
    let (speaker_tx, amp_en) = if VOICES3R {
        match (|| -> Result<_> {
            let mut audio_i2c = I2cDriver::new(
                p.i2c1,
                p.pins.gpio45,
                p.pins.gpio0,
                &I2cConfig::new().baudrate(Hertz(400_000)),
            )?;
            es8311::probe_bus(&mut audio_i2c);
            // 順番が命 (Refs #102): I2S で BCK/WS を流してからコーデックを起こす
            let mut spk = speaker::Speaker::new(
                p.i2s1,
                p.pins.gpio17.into(), // BCLK
                p.pins.gpio3.into(),  // WS (LRCK)
                p.pins.gpio48.into(), // DOUT
            )?;
            spk.feed_silence(300)?;
            let en = es8311::init_amp(&mut audio_i2c, p.pins.gpio18.into())?;
            Ok((speaker::start_player(spk)?, en))
        })() {
            Ok((tx, en)) => (Some(tx), Some(en)),
            Err(e) => {
                log::warn!("speaker: 初期化失敗 — 音なしで継続する: {e:#}");
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // 本機は画面を持たないので、push_event の行き先は捨て場。
    // それでも `nfc::start` はボード非依存の口として status を要求する
    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));

    // 測定台 (Atom VoiceS3R) だけが持つ口: ホストコンソール (PC のブラウザが話す)
    // と血圧計。既定 (AtomS3 Lite のベンチ検証機) はどちらも持たず、従来どおり
    // 読み取りログを出すだけ。
    //
    // 起動順は atoms3-timecard と同じ「Settings → console → NFC → BLE」。
    // console を NFC より先に立てるのは、Unit NFC が挿さっていなくても
    // PC 側から `PING` / `LOG DUMP` が返るようにするため
    let station = if VOICES3R {
        let settings = Settings::new(EspDefaultNvsPartition::take()?)?;
        // hub-ble はスキャンのたびに status 側の写しを読む
        // (`console::handle_omron` が切り替え時に更新する)
        if let Ok(mut st) = status.lock() {
            st.omron_bp = settings.omron_bp();
        }
        // 再ペアリング要求のフラグ。console (`PAIR`) が立て、BLE ループが消費する。
        // **両方に同じものを渡す** — 別物を渡すと画面のボタンが何も起こさない
        let pair_flag = alc_hub_common::control::new_pair_flag();
        // 本機は `STATUS` も `OTA` も持たない (ホストリンクが無い) ので機種固有の
        // 分岐がゼロになる。**4 本目の console.rs を作らず**共通の入口を呼ぶ
        // (hub-drivers/src/console.rs の `start_common`)
        console::start_common(
            "nfc",
            Arc::clone(&status),
            settings.clone(),
            Arc::clone(&pair_flag),
        )?;
        Some((settings, pair_flag))
    } else {
        None
    };

    // Unit NFC (ST25R3916): Grove Port A (SDA=G2 / SCL=G1)。読み取りループと
    // 重複抑止 (TapGate) は hub-drivers/src/nfc.rs が持つ。
    // **ここに NFC のコードを書かないこと** (issue #146)
    let led_for_nfc = Arc::clone(&led);
    nfc::start(
        I2C_PORT_NFC,
        p.pins.gpio2.into(),
        p.pins.gpio1.into(),
        // 検証機は従来どおり F → A → B (#155 step 4 の B 先行は本番機 atoms3-timecard だけ)
        nfc::PollOrder::FelicaFirst,
        // 存在検知ゲートも従来どおり (#175 の AlwaysPoll は本番機 atoms3-timecard だけ)
        nfc::PresenceGate::Adaptive,
        Arc::clone(&status),
        move |e: &NfcEvent| notify_event(&led_for_nfc, speaker_tx.as_ref(), e),
    )?;
    // 起動直後のログは取りこぼすことがあるので、音の有無は NFC の起動後に出す
    if VOICES3R {
        log::info!(
            "測定台 (Atom VoiceS3R): LED なし / 読めたらビープ = {}",
            if amp_en.is_some() {
                "有効"
            } else {
                "無効 (初期化失敗)"
            }
        );
    }

    // 血圧計 (Omron HEM-6231T) — **`OMRON BP ON` のときだけ**。
    // 起動時の設定で立てるかどうかを決めるので、OFF → ON には再起動が要る
    // (理由と中身の所在は `start_bp` の doc)
    if let Some((settings, pair_flag)) = &station {
        start_bp(&status, settings, pair_flag)?;
    }

    // メインループは LED のラッチ戻しだけ。検知そのもののログは nfc.rs が出す
    loop {
        FreeRtos::delay_ms(50);
        if let Ok(mut led) = led.lock() {
            led.expire();
        }
    }
}

/// 血圧計 (Omron HEM-6231T) の配線 — **`ble` feature = 測定台 (VoiceS3R) の
/// build だけ**に入る。
///
/// BLE central の中身 (scan / bond / 鍵登録 / 0x2A35 のデコード) は CoreS3 /
/// タイムカード端末と同じ `alc_hub_ble`、測定値の JSON 化・重複排除も同じ
/// `recorder` を通す。**ここに血圧のコードを書かないこと** — 機種で割れると
/// 「CoreS3 では届くのに測定台では届かない」になる
/// (`crates/atoms3-timecard/src/main.rs` の同名の節と同型)。
///
/// **BLE を起動時の設定で立てるかどうか決めている** ので `OFF → ON` の切り替えには
/// 再起動が要る (OFF のまま起動したときは `EVT BLE_DISABLED` を出す)。
/// BT controller は内部RAM を使うため、血圧計を使わない台で常時初期化すると
/// NFC の読み取りに要る内部RAM を削る。
#[cfg(feature = "ble")]
fn start_bp(status: &SharedStatus, settings: &Settings, pair_flag: &PairFlag) -> Result<()> {
    if !settings.omron_bp() {
        // 既定。**BT controller ごと起こさない**
        alc_hub_common::evtlog::emit("EVT BLE_DISABLED omron_bp=0");
        return Ok(());
    }
    let (meas_tx, meas_rx) = mpsc::channel();
    // `recorder` の送り先は 3 つ (ホストへの JSON / 上り WS / Windows GW) だが、
    // **本機に残るのはホストへの JSON だけ** — 画面も uplink も GW も持たない
    // (サーバへ届けるのは PC 側の画面の仕事)。受け側を持たない channel への
    // 送信は `recorder` も `hub-ble` も `let _ = tx.send(..)` で捨てるので、
    // ここで受け側を drop する。**保持して読み捨てにしないこと** — 誰も読まない
    // キューを毎周回す手間が増えるだけで、溜まり続ける方が危ない
    let (ui_tx, ui_rx) = mpsc::channel();
    drop(ui_rx);
    let (ws_tx, ws_rx) = mpsc::channel();
    drop(ws_rx);
    // 測定値レコーダ (BLE の notify コールバックを軽量に保つ専用スレッド)
    recorder::start(
        meas_rx,
        ui_tx.clone(),
        Arc::clone(status),
        settings.clone(),
        ws_tx,
        None,
    )?;
    // 本機は Wi-Fi を持たないので電波の取り合いは起きない
    let coex = Arc::new(alc_hub_core::coex::RadioCoex::new());
    alc_hub_ble::start(
        Arc::clone(status),
        meas_tx,
        ui_tx,
        coex,
        Arc::clone(pair_flag),
        settings.clone(),
    )?;
    alc_hub_common::evtlog::emit("EVT BLE_ENABLED omron_bp");
    Ok(())
}

/// 既定 (AtomS3 Lite のベンチ検証機) の build。**BLE そのものが入らない** —
/// あちらの `sdkconfig.defaults` は BT を持たず esp32-nimble がリンクできない
/// (ファイル冒頭の「血圧計用 PC の測定台」節)。`VOICES3R` が偽であることは
/// 冒頭の const アサーションが保証しているので、この関数は呼ばれない
#[cfg(not(feature = "ble"))]
fn start_bp(_status: &SharedStatus, _settings: &Settings, _pair_flag: &PairFlag) -> Result<()> {
    Ok(())
}

/// 検知結果を LED の色にする。**2 枚見え (#143) と読み取り失敗は赤** —
/// どちらも「かざしたのに登録されなかった」ことを目視で分ける必要がある。
/// 音を持つ build (VoiceS3R の測定台) では、読めたとき (緑) だけ短いビープを鳴らす
fn notify_event(led: &Mutex<Led>, speaker: Option<&mpsc::Sender<Sound>>, event: &NfcEvent) {
    let color = match event {
        NfcEvent::ReadFailed { .. } | NfcEvent::MultipleCards => LED_ERR,
        NfcEvent::Felica { .. }
        | NfcEvent::NfcaUid { .. }
        | NfcEvent::CarInspection { .. }
        | NfcEvent::License { .. } => LED_OK,
    };
    if color == LED_OK {
        if let Some(tx) = speaker {
            // 再生は speaker スレッド。ここはキューに積むだけで NFC を止めない
            let _ = tx.send(Sound::BeepOk);
        }
    }
    if let Ok(mut led) = led.lock() {
        led.paint(color);
    }
}

/// 本体 LED (WS2812) とラッチ状態。**塗るのは NFC スレッド (sink クロージャ)、
/// 待機色へ戻すのは main ループ**なので Mutex 越しに共有する。
/// **本番機 (crates/atoms3-timecard = Atom VoiceS3R) には LED が無い**ので
/// 共有相手はもう居ない (#151 で向こうの led.rs は消えた)。ここは AtomS3 Lite
/// (G35) 専用のベンチ用目視デバッグ
struct Led {
    /// `None` = LED を出さない build (`VOICES3R`)
    tx: Option<TxRmtDriver<'static>>,
    /// 現在出している色 (同色の再送出を避ける)
    shown: (u8, u8, u8),
    since: Instant,
}

impl Led {
    fn new(tx: Option<TxRmtDriver<'static>>) -> Self {
        let mut led = Self {
            tx,
            // 待機色以外にしておき、初回 paint で必ず 1 回描かせる
            shown: LED_OK,
            since: Instant::now(),
        };
        led.paint(LED_IDLE);
        led
    }

    fn paint(&mut self, color: (u8, u8, u8)) {
        self.since = Instant::now();
        if self.shown == color {
            return;
        }
        self.shown = color;
        let Some(tx) = self.tx.as_mut() else {
            return;
        };
        if let Err(e) = write_ws2812(tx, color) {
            log::warn!("led: write failed: {e:#}");
        }
    }

    /// ラッチを過ぎていれば待機色へ戻す
    fn expire(&mut self) {
        if self.shown != LED_IDLE && self.since.elapsed() > LATCH {
            self.paint(LED_IDLE);
        }
    }
}

/// WS2812 へ 1 ピクセル分の (R,G,B) を送る (esp-idf-hal 公式 rmt_neopixel 例に準拠)。
/// GRB 順で 24bit を MSB から送出する
fn write_ws2812(tx: &mut TxRmtDriver<'static>, (r, g, b): (u8, u8, u8)) -> Result<()> {
    let color: u32 = ((g as u32) << 16) | ((r as u32) << 8) | b as u32;
    let ticks_hz = tx.counter_clock()?;
    let t0h = Pulse::new_with_duration(ticks_hz, PinState::High, &Duration::from_nanos(350))?;
    let t0l = Pulse::new_with_duration(ticks_hz, PinState::Low, &Duration::from_nanos(800))?;
    let t1h = Pulse::new_with_duration(ticks_hz, PinState::High, &Duration::from_nanos(700))?;
    let t1l = Pulse::new_with_duration(ticks_hz, PinState::Low, &Duration::from_nanos(600))?;
    let mut signal = FixedLengthSignal::<24>::new();
    for i in (0..24u32).rev() {
        let bit = (color >> i) & 1 != 0;
        let (high, low) = if bit { (t1h, t1l) } else { (t0h, t0l) };
        signal.set(23 - i as usize, &(high, low))?;
    }
    tx.start_blocking(&signal)?;
    Ok(())
}
