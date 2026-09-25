//! alc-hub-atoms3-timecard: NFC タイムカード端末 (ippoan/alc-app-s3#134)。
//!
//! 営業所の出入口に常設し、カードをかざすと打刻イベント
//! (`kind = "timecard"`) を既存の WS uplink へ積むだけの端末。
//! **出勤/退勤の判定はしない** — 「誰が・いつ・どの端末で」だけを送り、
//! 判定は front 側で行う (plan/standing-devices.md §3.3)。
//! CoreS3 統合ハブ (ルートの alc-hub-cores3) と hub-* クレート群を共有する。
//!
//! # スコープ — 音はまだ入れない
//!
//! NFC → WS 送信までの経路。**音 (ES8311) は入れない** — 本番機 (Atom
//! VoiceS3R) への移行 (#151) と分けて別 issue で足す。したがって本 crate は
//! `alc-hub-drivers` の `nfc` feature のみを有効にし、`speaker` は使わない。
//!
//! # ハード構成 (本番機)
//!
//! - **Atom VoiceS3R** (M5Stack Atom EchoS3R, SKU C126-ECHO /
//!   ESP32-S3-PICO-1-N8R8): 8MB Flash + **8MB Octal PSRAM**。
//!   PSRAM の線モードは **OCT** — CoreS3 の QUAD をそのまま持ってくると
//!   `CONFIG_SPIRAM_IGNORE_NOTFOUND` により黙って PSRAM なしで起動する
//!   (根拠と検出方法は `sdkconfig.defaults` の PSRAM 節)
//! - **Atomic PoE Base** (SKU A091): W5500 SPI Ethernet + PoE 給電。
//!   SCLK=G5 / MISO=G7 / MOSI=G8 / CS=G6、INT/RST 未配線 (polling)
//! - **M5 Unit NFC** (U216, ST25R3916): Grove Port A (SDA=G2 / SCL=G1)
//!
//! ピンの根拠 (2026-09-05 に確認)。**確定と推定を分けて書く**:
//!
//! - **【確定・VoiceS3R 固有】内蔵オーディオ** (ES8311/NS4150B) は
//!   G45/G0/G48/G4/G3/G17/G11/G18、**IR_TX は G47**、**本体ボタンは G41**
//!   (`docs.m5stack.com/en/core/Atom_EchoS3R` の公式ピンマップ)。
//!   どれも底面バスにも Grove にも出ない
//! - **【確定・VoiceS3R 固有】Grove (HY2.0-4P) は SDA=G2 / SCL=G1**。
//!   公式ピンマップ (黄=G2 / 白=G1) と、M5 公式ファーム
//!   (`m5stack/uiflow-micropython`) の `M5STACK_Atom_EchoS3R/mpconfigboard.h`
//!   (`MICROPY_HW_I2C0_SCL 1` / `MICROPY_HW_I2C0_SDA 2`)、および M5Unified の
//!   `_pin_table_i2c_ex_in` の `board_M5AtomVoiceS3R` 行が一致する。
//!   内蔵 I2C は G0/G45 なので Unit NFC と競合しない
//! - **【確定・実機 2026-09-06】底面バスは J5 = 3V3/G5/G6/G7/G8、J6 = G39/G38/5V/GND**。
//!   もとは AtomS3R の回路図 (`Sch_M5_AtomS3R_v0.4.1.pdf`、M5 が C126-ECHO の
//!   SKU ページで本体基板の回路図として挙げているもの) **からの推定**だった —
//!   その PDF は VoiceS3R が持たない LCD と IMU を含むため同一基板とは限らない。
//!   **#151 の初回書き込みで Atomic PoE Base を載せた実機が W5500 に応答し
//!   DHCP まで通ったので確定**した (`EVT ETH_CONNECTED`)。
//!   外していれば `W5500 version mismatched` で必ず落ちる種類の推定だったので、
//!   実機まで持ち込んで決着させてよかった (経緯は plan §3.1)
//!
//! # ★ 本機で `led.rs` (WS2812) は成立しない
//!
//! **この基板に単線接続の WS2812 系 LED は無い。**回路図に出てくる LED は
//! すべて **I2C の LP5562 (アドレス 0x30、SYS_SDA=G45 / SYS_SCL=G0) 駆動**
//! (`LED_R` / `LED_G` / `LED_B` / `LED_BL_DRV`) で、RMT で 1 本の GPIO に
//! ビット列を流す `led.rs` の方式では**どうやっても点かない**。
//! LP5562 = 0x30 は M5GFX の `Light_M5StackAtomS3R` が AtomS3R の LCD
//! バックライトを叩いているアドレスと同じ。よって #151 で `led.rs` は削除した。
//!
//! **VoiceS3R に RGB LED が実装されているか自体は未確定。**
//! M5 公式 SKU ページ (C126-Echo) の比較表は
//! "Atom VoiceS3R has no RGB LED, while Atom Voice includes WS2812 x1" と
//! 書いており、公式ピンマップにも LED の項が無い — が、上の回路図には
//! LP5562 の回路がある (AtomS3R では実装されている)。
//! **決着は実機で内蔵 I2C の 0x30 を probe するしかない。**
//! なお M5Unified の RGBLED ピン表 `_pin_table_other0` に
//! `board_M5AtomVoiceS3R` が無いことは**根拠にならない** — あの表は単線
//! WS2812 用で、LP5562 で LED を持つ `board_M5AtomS3R` も同じく載っていない。
//!
//! 当面 **検知の可否は serial ログ (`EVT TIMECARD` / `EVT NFC_MULTI_CARD`)
//! で見る。**現場向けの可視/可聴フィードバックは ES8311 の音を入れる別 issue で戻す。
//!
//! # 血圧計 (Omron HEM-6231T) — **既定 OFF** (#135 / #237)
//!
//! `OMRON BP ON` (NVS `omron_bp`) のときだけ BLE central (`alc_hub_ble`) を
//! 起こし、受けた測定を CoreS3 と同じ `recorder` 経由で `kind="blood_pressure"`
//! として上り (cf-alc-recorder) へ積む。**スキャン・ボンド・鍵登録・デコードは
//! CoreS3 と同一の crate** で、ここは配線だけ。
//!
//! **BLE を起動時の設定で立てるかどうか決めている** — BT controller は内部RAM を
//! 使うため、打刻だけの端末で常時初期化すると `ws_uplink` の TLS ゲート
//! (内部RAM 60KB) を削る。したがって `OFF → ON` の切り替えには**再起動が要る**
//! (OFF のまま起動したときは `EVT BLE_DISABLED` を出す)。CoreS3 は BLE を常に
//! 起こすので再起動不要 — 差は「打刻端末では血圧が従」であることから来る。
//!
//! # Vein Station (`station` feature、#272)
//!
//! LAN なし・USB 1 本で Windows PC につなぐ構成。**W5500 を起こさず**、同じ
//! G7/G8 を FC-1200 の RS232 (MAX3232 経由、TX=G7 / RX=G8、9600 8N1) に回す。
//! FC-1200 のドライバは CoreS3 と同じ `rs232` で、測定値は `recorder` が
//! JSON 行で USB へ出す (血圧計の有無に関わらず常時起動)。**ws_uplink と NTP は
//! 起こさない** — LAN が無いので送信キューが NVS に溜まり続けるだけになる。
//! 送信キューの受け側はメインループで読み捨てる。bin 名と `HostKind` は PoE 版と
//! 同じなので、起動時の `EVT STATION …` で見分ける。
//!
//! # 指静脈 (`vein` feature、ippoan/vein-match#20)
//!
//! `station` に Waveshare Finger Vein Module を足す。**UART2** (FC-1200 が
//! UART1 を使うため) を TX=G5 / RX=G6、57600bps 8N1 で開き、`VEIN CAPTURE` で
//! 特徴量を読んで `VEIN CHARA <hex>` の 1 行で USB へ出す (手順は
//! `alc_hub_core::vein`、UART は `alc_hub_drivers::vein`)。案内音声 (`VEIN SAY`)
//! はホストが鳴らす。ピンは `main` の `let vein = …` の 1 か所だけで決める —
//! **中継基板 (vein-base) は未発注で変わりうる**。G5/G6 は PoE 版では W5500 の
//! SCLK/CS だが、`station` は W5500 を起こさないので空いている。
//!
//! # 起動順 (変えてはいけない)
//!
//! `crashlog::init` → `Settings::new` → `heap::start` → (`vein` なら `vein::start`) → `console::start` →
//! `ota::spawn_serial_confirm_watch` → `ws_uplink::start` → LAN → (任意) BLE (OTA の確定は ws_uplink が
//! 初回の WS 接続で行う。シリアル OTA (`OTA SERIAL`、Refs #279) で入れた image はホストの
//! `OTA CONFIRM` で確定し、10 分来なければ見張りが前の image へ戻す)。**`crashlog::init` は
//! `heap::start` より前**。配線漏れで `.noinit` のゴミ帳簿に書いて boot loop に
//! なった実害が 2026-07-14 にある。

mod console;

use alc_hub_common::{
    config,
    measurement::UplinkRecord,
    settings::Settings,
    status::{epoch_ms, now_ms, HubStatus, SharedStatus},
};
use alc_hub_drivers::nfc::NfcEvent;
use alc_hub_drivers::speaker::Sound;
use alc_hub_drivers::timecard::Punch;
use alc_hub_drivers::{crashlog, es8311, heap, nfc, ota, recorder, speaker};
#[cfg(feature = "station")]
use alc_hub_drivers::rs232;
#[cfg(feature = "vein")]
use alc_hub_drivers::vein;
#[cfg(not(feature = "station"))]
use alc_hub_drivers::{eth_w5500, ntp, ws_uplink};
use anyhow::Result;
#[cfg(not(feature = "station"))]
use esp_idf_svc::eventloop::EspSystemEventLoop;
#[cfg(not(feature = "station"))]
use esp_idf_svc::hal::spi::{config::DriverConfig as SpiDriverConfig, Dma, SpiDriver};
use esp_idf_svc::hal::{
    delay::FreeRtos,
    i2c::{config::Config as I2cConfig, I2cDriver},
    peripherals::Peripherals,
    units::Hertz,
};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use std::sync::{mpsc, Arc, Mutex};

/// nfc_shim (C++ 側) に立てさせる I2C ポート。本機は他に I2C を使わないので
/// I2C_NUM_0 (atoms3-nfc のベンチと同値、実機確認済み)。CoreS3 は内部バスが
/// I2C_NUM_0 を使うので向こうは 1
const I2C_PORT_NFC: i32 = 0;

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    // 前回リセットの解析 + ログ捕捉 hook (CoreS3 と同じ crashlog 基盤 #43)。
    // heap.rs の note() がリングに書くため、heap::start より前に必ず呼ぶこと
    let (_, crash) = crashlog::init();
    log::info!(
        "alc-hub-atoms3-timecard v{} 起動",
        config::firmware_version_full()
    );

    let p = Peripherals::take()?;
    #[cfg(not(feature = "station"))]
    let sysloop = EspSystemEventLoop::take()?;

    // NVS (device credential 等の永続設定)
    let nvs_partition = EspDefaultNvsPartition::take()?;
    let settings = Settings::new(nvs_partition)?;
    // 前の起動で OTA 直後の image を戻していたら、その証跡を出す (Refs #217)。
    // station は ws_uplink を起こさないので、ここで出す (ws_uplink::start の分は空振り)
    ota::report_previous_rollback(&settings);

    // Omron 血圧計を拾うか (`OMRON BP ON|OFF`、NVS `omron_bp`、**既定 OFF**)。
    // hub-ble はスキャンのたびにこの写しを読む (console::handle_omron が更新する)
    let omron_bp = settings.omron_bp();
    let status: SharedStatus = Arc::new(Mutex::new(HubStatus {
        omron_bp,
        ..HubStatus::default()
    }));
    // ヒープ監視 (OOM 捕捉 + low-water 計測) は重いアロケーションより先に登録
    heap::start(Arc::clone(&status))?;

    // 指静脈モジュール (UART2)。console が `VEIN CAPTURE` / `VEIN SAY` を渡すので
    // console より先に立てる。音の送り口は speaker が立ってから入れる (下)
    #[cfg(feature = "vein")]
    let vein = {
        // ★ 指静脈の UART ピンは**ここ 1 か所**。中継基板 (vein-base、J1-2 = G5 →
        //   モジュール RXD / J1-3 = G6 ← モジュール TXD) は未発注で変わりうる
        let (tx, rx) = (p.pins.gpio5, p.pins.gpio6);
        let pins = {
            use esp_idf_svc::hal::gpio::Pin;
            (tx.pin(), rx.pin())
        };
        let link = vein::start(p.uart2, tx, rx)?;
        alc_hub_common::evtlog::emit(&format!(
            "EVT STATION VEIN TX=G{} RX=G{} BAUD={}",
            pins.0,
            pins.1,
            vein::BAUD
        ));
        link
    };

    // ホストコンソール (PING / STATUS / HEAP / OTA / AUTH / WS)
    // 再ペアリング要求のフラグ。console (`PAIR`) が立て、BLE ループが消費する。
    // **両方に同じものを渡す** — 別物を渡すと Pages のボタンが何も起こさない
    let pair_flag = alc_hub_common::control::new_pair_flag();
    console::start(
        Arc::clone(&status),
        settings.clone(),
        Arc::clone(&pair_flag),
        #[cfg(feature = "vein")]
        vein.clone(),
    )?;
    // シリアル OTA で入れた image の確定待ち (Refs #279)。印が無ければ何もしない
    ota::spawn_serial_confirm_watch(settings.clone());

    // cf-alc-recorder への WS 常時接続。打刻イベントはここへ積む。
    // 接続には AUTH SET 済み credential と LAN 接続が必要 (未登録の間は
    // 接続しないだけで無害 — 送信キューは NVS 永続なので打刻は失わない)
    let (ws_meas_tx, ws_meas_rx) = mpsc::channel();
    // 本機は画面を持たないので UiCommand は捨てる。ただし **受け側を保持したまま
    // 読み捨てないこと** — drop すると ws_uplink スレッドが channel 切断で終了し、
    // 放置すると届いた分がキューに溜まり続ける。**下のメインループで毎周
    // 読み捨てる** (専用スレッドは立てない — 8KB の内部RAM を使わないため)。
    // 送り手は ws_uplink / recorder / hub-ble の 3 つ
    let (ui_tx, ui_rx) = mpsc::channel();
    let recorder_ui_tx = ui_tx.clone();
    let ui_tx_for_ble = ui_tx.clone();
    #[cfg(feature = "station")]
    let rs232_ui_tx = ui_tx.clone();
    // boot_id は NTP 未同期で記録した測定の時刻補正に使う (ws_uplink.rs)。
    // **打刻は時刻が命**なので、この補正は本機でこそ効く
    #[cfg(not(feature = "station"))]
    ws_uplink::start(
        ws_meas_rx,
        ui_tx,
        Arc::clone(&status),
        settings.clone(),
        settings.next_boot_id(),
    )?;

    // 前回がクラッシュ由来なら panic 前ログを kind=crash_log で送信キューへ
    if let Some(snap) = &crash {
        crashlog::report(snap, &ws_meas_tx, &status);
    }

    // W5500 (Atomic PoE Base): SCLK=G5 / MISO=G7 / MOSI=G8 / CS=G6。
    // DMA 必須 — 無効だと SPI 転送が 64 バイト上限になり、Ethernet フレーム
    // (最大 ~1.5KB) の read/write が "spi transmit failed" で全滅する
    #[cfg(not(feature = "station"))]
    let spi = SpiDriver::new(
        p.spi2,
        p.pins.gpio5,
        p.pins.gpio8,
        Some(p.pins.gpio7),
        &SpiDriverConfig::new().dma(Dma::Auto(4096)),
    )?;
    // leak して 'static 参照で渡す (eth_w5500::start の doc コメント参照)
    #[cfg(not(feature = "station"))]
    let spi: &'static SpiDriver<'static> = Box::leak(Box::new(spi));
    #[cfg(not(feature = "station"))]
    eth_w5500::start(spi, p.pins.gpio6.into(), None, sysloop, Arc::clone(&status))?;

    // 内蔵オーディオ (ES8311 + NS4150B、issue #154)。**打刻音はこの端末で
    // かざした人に伝わる唯一の反応** — 本機に LED は無い (#151)。
    //
    // I2C は内蔵バス (SDA=G45 / SCL=G0)。**Grove の Unit NFC とは別ポート**で、
    // あちらは nfc_shim (C++) が I2C_NUM_0 を新ドライバで握っているため
    // ここは i2c1 を使う (CoreS3 とは逆の割り当て)
    let mut audio_i2c = I2cDriver::new(
        p.i2c1,
        p.pins.gpio45,
        p.pins.gpio0,
        &I2cConfig::new().baudrate(Hertz(400_000)),
    )?;
    // 内蔵バスに誰が居るか。ES8311 (0x18) が見えなければ配線かポートが違う。
    // ついでに LP5562 (0x30) の有無も見る — #151 で未確定のまま残した点で、
    // 居なければ「この端末に RGB LED は無い」が確定する
    es8311::probe_bus(&mut audio_i2c);

    // 音は**失敗しても致命にしない** — 鳴らなくても打刻そのものは成立する。
    // `_amp_en` は NS4150B の有効化ピン (G18) で、**drop すると出力が落ちる**ので
    // main が持ち続ける
    let (speaker_tx, _amp_en) = match (|| -> Result<_> {
        // **順番が命** (Refs #102): I2S を立てて BCK/WS を実際に流してから
        // コーデックを起こす。新 I2S ドライバは FIFO 空で BCK を止めるので、
        // `tx_enable()` だけではクロックが出ず PLL がロックしない。
        // MCLK (G11) は配線しない — ES8311 側を MCLK=BCLK で使う (es8311.rs)
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
            log::warn!("speaker: 初期化失敗 — 打刻音なしで継続する: {e:#}");
            alc_hub_common::evtlog::emit("EVT SPEAKER_NG");
            (None, None)
        }
    };

    // `VEIN SAY` の音はこの再生スレッドで鳴らす (初期化に失敗したら入れない
    // = `ERR VEIN NO_SPEAKER`)
    #[cfg(feature = "vein")]
    if let Some(tx) = &speaker_tx {
        vein.set_speaker(tx.clone());
    }

    // 血圧の上り経路も打刻と同じ送信キューへ積む。**下の nfc::start が
    // ws_meas_tx 本体を move する**ので、ここで clone を取っておく
    let ws_for_bp = ws_meas_tx.clone();

    // Unit NFC (ST25R3916): Grove Port A (SDA=G2 / SCL=G1)。読み取りループは
    // hub-drivers/src/nfc.rs (CoreS3 と共有)。**ここに NFC のコードを書かない**
    nfc::start(
        I2C_PORT_NFC,
        p.pins.gpio2.into(),
        p.pins.gpio1.into(),
        // B 先行 + B 粘着 (#155 step 4)。理由は nfc::PollOrder の doc
        nfc::PollOrder::LicenseFirst,
        // 存在検知をゲートに使わず待機中も B を回す (#175)。本番機では免許証を上から置いても
        // 振幅・位相が動かない置き方があり、ゲートが 1/5 しか開かなかった。理由は nfc::PresenceGate の doc
        nfc::PresenceGate::AlwaysPoll,
        Arc::clone(&status),
        move |e: &NfcEvent| on_card(e, &ws_meas_tx, speaker_tx.as_ref()),
    )?;

    // 血圧計 (Omron HEM-6231T) — **`OMRON BP ON` のときだけ**。
    //
    // BLE central の中身 (scan / bond / 鍵登録 / 0x2A35 のデコード) は CoreS3 と
    // 同じ `alc_hub_ble`、測定値の JSON 化・重複排除・上りへの fan-out も同じ
    // `recorder` を通す。**ここに血圧のコードを書かない** — 機種で割れると
    // 「CoreS3 では届くのに VoiceS3R では届かない」になる。
    //
    // 起動時の設定で立てるかどうかを決める理由と、OFF → ON に再起動が要ることは
    // このファイル冒頭の「血圧計」節を参照
    let (meas_tx, meas_rx) = mpsc::channel();
    // 測定値レコーダ (BLE の notify コールバックを軽量に保つ専用スレッド)。
    // 本機に画面も alc-gw への生中継も無いので UI は捨て、GW は None。
    // station は FC-1200 の測定値もここを通すので血圧計の有無に関わらず起こす
    if omron_bp || cfg!(feature = "station") {
        recorder::start(
            meas_rx,
            recorder_ui_tx,
            Arc::clone(&status),
            settings.clone(),
            ws_for_bp,
            None,
        )?;
    }
    if omron_bp {
        // 再ペアリング要求は console の `PAIR` (Pages の「血圧計を再ペアリング」) が
        // 立てる。**起動時にボンドを消さない**ことが大事 (probe bin の作法を持ち込むと
        // 毎回ペアリングし直しになる)
        // 本機は Wi-Fi を持たないので電波の取り合いは起きない。OTA 中の一時停止は
        // hub-ble が status.ota_active を見て自前で行う
        let coex = Arc::new(alc_hub_core::coex::RadioCoex::new());
        alc_hub_ble::start(
            Arc::clone(&status),
            meas_tx.clone(),
            ui_tx_for_ble,
            coex,
            Arc::clone(&pair_flag),
            settings.clone(),
        )?;
        alc_hub_common::evtlog::emit("EVT BLE_ENABLED omron_bp");
    } else {
        // 既定。**BT controller ごと起こさない** = 打刻だけの端末の内部RAM を
        // 削らない。`OMRON BP ON` のあとは再起動が要ることをログに残す
        alc_hub_common::evtlog::emit("EVT BLE_DISABLED omron_bp=0");
    }

    // FC-1200 (RS232、MAX3232 経由): TX=G7 / RX=G8。PoE 版では W5500 の
    // MISO/MOSI に使っているピン。ドライバは CoreS3 と共有の rs232
    #[cfg(feature = "station")]
    {
        rs232::start(
            p.uart1,
            p.pins.gpio7,
            p.pins.gpio8,
            Arc::clone(&status),
            meas_tx,
            rs232_ui_tx,
        )?;
        alc_hub_common::evtlog::emit("EVT STATION RS232 TX=G7 RX=G8 LAN=0");
    }

    // SNTP。**打刻端末では必須** — 起動しないとシステム時刻が 1970 のままで、
    // 打刻の `recorded_at_ms` が 1970 起点で送られる (範囲内なので DB 側で NULL
    // にもならず、静かに 55 年ずれた打刻が入る)。`ws_uplink` の
    // `should_wait_for_clock` は 60 秒待って諦め、`fix_unsynced_times` は
    // 「あとで同期したら補正する」仕組みなので、同期が来なければ永久に発火しない。
    // **ここで即起動してはいけない** — 理由は ntp::start_when_online の doc
    #[cfg(not(feature = "station"))]
    let mut sntp = None;

    // メインループ: SNTP の遅延起動と UiCommand の読み捨てだけ (ホスト向け
    // イベントは eth_w5500 / heap / ws_uplink の各スレッドが出す)
    loop {
        FreeRtos::delay_ms(100);
        // 画面が無いので UiCommand は捨てる。**捨てないと溜まり続ける**
        // (受け側は上のとおり drop できない)
        while ui_rx.try_recv().is_ok() {}
        // station は ws_uplink を起こさないので送信キューも同じく読み捨てる
        // (drop すると打刻の送信失敗扱いで打刻音が鳴らなくなる)
        #[cfg(feature = "station")]
        while ws_meas_rx.try_recv().is_ok() {}
        #[cfg(not(feature = "station"))]
        ntp::start_when_online(
            &mut sntp,
            status.lock().map(|s| !s.lan_ip.is_empty()).unwrap_or(false),
        );
    }
}

/// カードを 1 枚読めたときの処理: 打刻イベントを送信キューへ積む。
///
/// `NfcEvent` → 打刻キー → `UplinkRecord` の判定は hub-drivers の
/// `timecard::Punch` (CoreS3 と共有、#188)。**音と `EVT …` の println は
/// ここに残す** — `ReadFailed` で鳴らさない / 送信キューへ積めたときだけ
/// 鳴らす (#155) の分岐を共有側に隠さないため。
///
/// **`card_id` は生値のまま**送る (接頭辞を付けると punch のカード照合が
/// 必ず外れる — alc_hub_core::timecard の doc 参照)。`session_id` は
/// 点呼ではないので付けない。
fn on_card(
    event: &NfcEvent,
    ws_tx: &mpsc::Sender<UplinkRecord>,
    speaker: Option<&mpsc::Sender<Sound>>,
) {
    let Some(punch) = Punch::from_event(event) else {
        // 打刻にしないイベント (どれを弾くかは Punch::from_event の doc)。
        //
        // **2 枚検知は黙って捨ててはいけない** (#155)。本機に LED は無いので、
        // 打刻しないと端末が**完全に無反応**になる。実機で 2 枚検知を利用者が
        // **「壊れている」と受け取った** (2026-09-06)。**断ったことを音で返す**
        //
        // `ReadFailed` は**ここでは鳴らさない** (#155)。「カードが載っている間の
        // 再読が失敗した」ときにも出る — 実機ログでは**打刻成功の直後に必ず**
        // `rc=-6 (READ BINARY 失敗)` / `rc=-4 (SELECT MF 失敗)` が続いている
        // (2026-09-06)。ここで鳴らすと**打刻できたのにエラー音が鳴り**、
        // 「断った」ことを伝えるどころか誤解を増やす。
        // 読めなかったときの無反応は、**かざし直せば済む**ぶん 2 枚検知より軽い
        if let NfcEvent::MultipleCards = event {
            alc_hub_common::evtlog::emit("EVT NFC_MULTI_CARD");
            if let Some(tx) = speaker {
                let _ = tx.send(Sound::PunchNg);
            }
        }
        return;
    };

    // 打刻時刻。NTP 未同期なら ws_uplink が送信時に稼働時間の差で補正する
    let recorded_at_ms = epoch_ms();
    let record = punch.record(now_ms(), recorded_at_ms);
    // 行の形は CoreS3 と共有 (alc_hub_core::timecard::evt_line)。#644 で CoreS3 も
    // 同じ行を出すようになったので、綴りを 2 か所に持たない (中身は従来と同一)
    println!("{}", alc_hub_core::timecard::evt_line(&punch.card_id, punch.kind));
    if ws_tx.send(record).is_err() {
        // ws_uplink スレッドが死んでいる = 送信不能。**鳴らさない** —
        // 「鳴った = 打刻を預かった」を崩さないため
        log::error!("timecard: 送信キューへ積めなかった (ws_uplink が停止)");
        return;
    }
    // 打刻音。**送信キューへ積めたときだけ鳴らす。**再生はスレッド分離済み
    // (speaker::start_player) なので、ここはキュー投入だけで即座に戻る —
    // NFC のポーリングを 160ms 止めない
    if let Some(tx) = speaker {
        let _ = tx.send(Sound::PunchOk);
    }
}
