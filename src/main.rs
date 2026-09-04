//! alc-hub-cores3: M5Stack CoreS3 統合ハブ ファームウェア (画面処理)
//!
//! `ippoan/alc-app` の plan/cores3-hub-consolidation.md (issues #100 / #102 の
//! 参照元) に基づく、点呼キオスク向け CoreS3 統合ハブの画面処理実装。
//!
//! クレート構成 (再コンパイル範囲の最小化と並列ビルドのための枝分かれ):
//!
//! ```text
//! hub-core (純粋) → hub-common (状態/設定/UIコマンド)
//!                     ├→ hub-ble   (体温計/血圧計)      ┐
//!                     ├→ hub-wifi  (Wi-Fi + Improv)     ├ 互いに独立 = 並列
//!                     ├→ hub-drivers (ホストリンク/RS232) ┘ (drivers→wifi)
//!                     └→ hub-ui    (画面。hub-board にも依存)
//! hub-board (ボード初期化, 独立葉)
//! 本クレート = main の配線のみ (ほぼ変更されない)
//! ```

use std::sync::{mpsc, Arc, Mutex};

use alc_hub_ble as ble;
use alc_hub_board as board;
use alc_hub_common::{
    config,
    settings::Settings,
    status::{HubStatus, SharedStatus},
};
#[cfg(feature = "lan")]
use alc_hub_drivers::lan;
use alc_hub_drivers::{crashlog, gw_link, heap, host_link, ntp, recorder, rs232, ws_uplink};
use alc_hub_ui as ui;
use alc_hub_wifi::{improv, wifi};
use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::{
    i2c::{config::Config as I2cConfig, I2cDriver},
    peripherals::Peripherals,
    spi::{config::DriverConfig as SpiDriverConfig, Dma, SpiDriver},
    units::Hertz,
};
use esp_idf_svc::nvs::EspDefaultNvsPartition;

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    // 前回リセットの解析 (クラッシュ由来なら panic 前ログの snapshot を得る) と
    // ログ捕捉 hook (vprintf tee + Rust panic hook) の設置。他モジュールの
    // 初期化より先に呼び、起動中のログ・クラッシュも捕まえる (Refs #43)
    let crash = crashlog::init();
    log::info!("alc-hub-cores3 v{} 起動", config::FIRMWARE_VERSION);

    let p = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;

    // NVS (BLE/Wi-Fi スタックも使用) と永続設定 (画面向き・Wi-Fi 認証情報)
    let nvs_partition = EspDefaultNvsPartition::take()?;
    let settings = Settings::new(nvs_partition.clone())?;

    // 内部 I2C (SDA=G12 / SCL=G11): AXP2101 / AW9523 / FT5x06 (タッチ)
    let i2c_cfg = I2cConfig::new().baudrate(Hertz(400_000));
    let mut i2c = I2cDriver::new(p.i2c0, p.pins.gpio12, p.pins.gpio11, &i2c_cfg)?;

    // 電源 (LCD バックライト・リセット含む) → LCD の順で初期化。
    // M-Bus/SPI2 バス (SCK=G36 / MISO=G35 / MOSI=G37) は LCD (CS=G3) と
    // Base LAN PoE v1.2 の W5500 (CS=G9、lan.rs) が共有する。G35 は LCD の
    // DC と二役 (display.rs SharedDcInterface 参照)。DMA 必須 — 無効だと
    // Ethernet フレーム転送が 64 バイト上限で全滅する (atoms3-print の実機知見)
    board::power::init(&mut i2c)?;
    // ボード種別 (CoreS3 / CoreS3 SE): SE は RTC (0x51) も IMU (0x69) も無い。
    // 同じバイナリで動くが、バッテリー表示のゲートと STATUS BOARD= の報告に使う
    // (plan/cores3-hub-consolidation.md「次期構成: CoreS3 SE + Base LAN PoE v1.2」)
    let probe = board::board::probe(&mut i2c);
    let board_kind = alc_hub_core::board::BoardKind::from_probe(probe.rtc_present, probe.imu_present);
    log::info!(
        "board: {} (rtc={} imu={})",
        board_kind.label(),
        probe.rtc_present,
        probe.imu_present
    );
    // M-Bus 5V を Core 側から出すか (AW9523 BUS_EN)。**バッテリーレスの
    // CoreS3 SE では出さない**。SE は Base LAN PoE v1.2 のように自前で M-Bus 5V を
    // 供給するベースを履く常設機の構成 (plan/cores3-hub-consolidation.md「次期構成」)
    // で、両側から同じ 5V レールを駆動すると PoE 単独給電では起動できない
    // — 電池が無く突入を吸収できないため。USB を挿していると VBUS が支えるので
    // 気づけず、「工場出荷ファームは PoE で動くのに焼いたファームだけ動かない」
    // という症状になる。バッテリー付きの CoreS3 は従来どおり Core 側から供給する
    // (USB 給電のベンチで RS232M/LAN 13.2 に電源が要る、Refs #76)
    board::power::set_ext_5v_out(&mut i2c, board_kind != alc_hub_core::board::BoardKind::CoreS3Se)?;
    let rotation = settings.rotation();
    // 起動カウンタを 1 つ進める (点呼セッション ID の前置、Refs #112)。
    // **起動ごとに 1 回だけ** — 再起動をまたいだ session_id の再利用を防ぐ。
    // settings は後段の host_link へ move されるので、ここで取っておく。
    let boot_id = settings.next_boot_id();
    let spi = SpiDriver::new(
        p.spi2,
        p.pins.gpio36,
        p.pins.gpio37,
        Some(p.pins.gpio35),
        &SpiDriverConfig::new().dma(Dma::Auto(4096)),
    )?;
    // LCD と W5500 で共有するため leak して 'static 参照で配る
    let spi: &'static SpiDriver<'static> = Box::leak(Box::new(spi));
    let display = board::display::init(spi, p.pins.gpio3, rotation)?;

    let status: SharedStatus = Arc::new(Mutex::new(HubStatus {
        board: board_kind,
        // 点呼の構成 (血圧はオプション、既定 OFF)。`TENKO BP` で NVS ごと更新される
        tenko_bp: settings.tenko_bp(),
        ..HubStatus::default()
    }));
    // ヒープ監視 (OOM 捕捉 + low-water 継続計測、Refs #27)。Wi-Fi/BLE/TLS の
    // 重いアロケーションより先に登録し、初期化中の OOM も捕まえる
    heap::start(Arc::clone(&status))?;
    // 永続化された測定ログを起動時に読み戻し、「ログ確認」画面に前回までの
    // 記録を表示する (リブートで測定記録が消えないようにする)
    if let Ok(mut st) = status.lock() {
        for line in settings.measurement_log() {
            st.events.push_back(line);
        }
    }

    let (tx, rx) = mpsc::channel(); // UiCommand: 各種 → UI ループ
    let (meas_tx, meas_rx) = mpsc::channel(); // Measurement: BLE → recorder

    // Wi-Fi (Improv Wi-Fi Serial で設定。保存済みなら起動時に自動接続)
    let wifi = wifi::Wifi::new(p.modem, sysloop.clone(), nvs_partition, Arc::clone(&status))?;
    let coex = wifi.coex_handle();
    let saved_credentials = settings.wifi_credentials();
    let provisioned = saved_credentials.is_some();
    if let Some((ssid, pass)) = saved_credentials {
        // 起動時接続 + 切断検出時の自動再接続を常駐スレッドで維持する。
        // (単発接続だと BLE との電波競合や AP 瞬断で一度切れると復帰しない)
        let wifi = wifi.clone();
        alc_hub_drivers::task::name_next(c"wifi_keepalive");
        std::thread::Builder::new()
            .name("wifi_keepalive".into())
            .stack_size(8 * 1024)
            .spawn(move || wifi.keepalive(ssid, pass))?;
    }
    let improv =
        improv::Improv::new(settings.clone(), wifi.clone(), Arc::clone(&status), provisioned);

    // BLE 再ペアリング要求フラグ (host_link の PAIR → ble タスクがボンド消去)
    let pair_flag = alc_hub_common::control::new_pair_flag();

    // 測定データの WS 送信 (cf-alc-recorder)。recorder が fan-out した測定を
    // NVS 永続キュー経由で送る (未ペアリング・圏外でも測定は失わない)
    let (ws_tx, ws_rx) = mpsc::channel();
    // boot_id は NTP 未同期で記録した測定の時刻補正にも使う (同じ起動の分だけ直す)
    ws_uplink::start(ws_rx, tx.clone(), Arc::clone(&status), settings.clone(), boot_id)?;

    // 前回がクラッシュ由来のリセットだったら、panic 前ログ + reset reason を
    // kind="crash_log" として送信キューへ積む (NVS 永続なので圏外でも失わない)
    if let Some(snap) = &crash {
        crashlog::report(snap, &ws_tx, &status);
    }

    // Windows GW (alc-gw) への LAN 内 WS 接続 (alc-app#120)。recorder が
    // fan-out した測定を生中継し、下り (点呼UI の測定開始) を受ける。
    // 接続先は `GW URL` コマンドで NVS 保存 (未設定なら何もしない)
    let (gw_tx, gw_rx) = mpsc::channel();
    gw_link::start(gw_rx, tx.clone(), Arc::clone(&status), settings.clone())?;

    // 測定値レコーダ (BLE コールバックを軽量に保つための専用スレッド):
    // JSON 出力 + NVS 記録 + 画面通知 + WS/GW fan-out を担う
    recorder::start(
        meas_rx,
        tx.clone(),
        Arc::clone(&status),
        settings.clone(),
        ws_tx,
        gw_tx,
    )?;

    // auth-worker device JWT 交換 (AUTH TOKEN 自己診断) は host_link が
    // auth_link::spawn_mint_test で一時スレッド起動する (常駐させない —
    // TLS 用 20KB スタックは診断中だけ確保。credential は AUTH SET で注入)
    host_link::start(
        tx.clone(),
        Arc::clone(&status),
        settings,
        wifi,
        pair_flag.clone(),
        improv,
    )?;
    // FC-1200 (RS232M Module 13.2) のピン。次期構成 (cores3-se feature) では
    // RS232M のジャンパを TX=G10 / RX=G6 へ移し、空いた Port C (G17/G18) を NFC
    // に回す (plan/cores3-hub-consolidation.md「次期構成」)。**feature とジャンパ
    // 位置がずれると FC-1200 と NFC が同じピンを取り合う** ので、書き込む
    // バイナリと実機の配線を必ず揃えること
    #[cfg(not(feature = "cores3-se"))]
    let (rs232_tx, rs232_rx) = (p.pins.gpio17, p.pins.gpio18);
    #[cfg(feature = "cores3-se")]
    let (rs232_tx, rs232_rx) = (p.pins.gpio10, p.pins.gpio6);
    rs232::start(
        p.uart1,
        rs232_tx,
        rs232_rx,
        Arc::clone(&status),
        meas_tx.clone(),
        tx.clone(),
    )?;
    // Unit NFC (ST25R3916) (issue #84 / #101)。DIN Base Port A (SDA=G2 / SCL=G1)
    // に配線 (AtomS3 ベンチと同一ピン番号)。SCL=G1 は Base LAN PoE v1.2 本体の
    // DB9 (TX=G1) と衝突するため、その DB9 は使わない。
    // I2C1 は C++ 側 (components/nfc_shim → M5HAL) が所有するため p.i2c1 は take しない
    // (I2C0=内部バス G12/G11 電源IC/タッチとは完全に別ポート)。
    // 内蔵スピーカー (I2S DOUT=G13) は Base LAN PoE v1.2 (CS=G9) とは競合しない。
    // 読み取りビープは issue #101 PR2
    #[cfg(feature = "nfc-verify")]
    {
        // 発音の成立条件 (issue #102 実機切り分けで確定):
        //   1. サンプルレートは 48kHz (44.1kHz は分数分周ジッタで AW88298 の
        //      PLL がロックせず完全無音。speaker.rs の SAMPLE_RATE_HZ 参照)
        //   2. アンプ初期化はクロック供給下で行う — 新 I2S ドライバは FIFO 空で
        //      BCK を止めるため、init_amp の前に feed_silence で実際に流す
        //
        // スピーカー初期化は**致命にしない**。nfc-verify が既定 on になって全機に
        // 載るため、ここで `?` を返すと AW88298 が黙っている個体が起動不能になり、
        // WS が上がらないので OTA でも戻せない (USB 復旧が要る)。音が出なくても
        // NFC の読み取り自体は成立するので、失敗時は受信側を落とした Sender を
        // 渡して無音で継続する (nfc.rs の beep_ok は send 失敗を無視する)
        let speaker_tx = match (|| -> Result<_> {
            let mut speaker = alc_hub_drivers::speaker::Speaker::new(
                p.i2s1,
                p.pins.gpio34.into(),
                p.pins.gpio33.into(),
                p.pins.gpio13.into(),
            )?;
            speaker.feed_silence(300)?;
            alc_hub_drivers::speaker::init_amp(&mut i2c)?;
            // 起動時セルフテスト音は鳴らさない (開発中は起動のたびに鳴って邪魔、
            // 2026-07-21 実機で発音経路は確認済み)。疎通は SYSST のログで代替:
            // feed_silence 中に PLL がロックしていれば bit0=PLLS / bit4=CLKS が立つ
            match alc_hub_drivers::speaker::read_sysst(&mut i2c) {
                Ok(v) => log::info!("speaker: SYSST(初期化後)=0x{v:04X}"),
                Err(e) => log::warn!("speaker: SYSST 読み出し失敗: {e:#}"),
            }
            // 再生専用スレッドに分離 (issue #102): I2S write はブロッキングのため
            // NFC スレッドで直接再生すると音声 1.5 秒ぶんポーリングが止まる
            alc_hub_drivers::speaker::start_player(speaker)
        })() {
            Ok(tx) => tx,
            Err(e) => {
                log::warn!("speaker: 初期化失敗 — NFC は無音で継続する: {e:#}");
                let (tx, _rx) = mpsc::channel();
                tx
            }
        };
        // 現行: Port A (SDA=G2 / SCL=G1)。次期構成 (cores3-se): Port C (G17/G18、
        // RS232M 退去で空く)。SDA/SCL の対応が未確定なので ack しなければ入替
        #[cfg(not(feature = "cores3-se"))]
        let (nfc_sda, nfc_scl) = (p.pins.gpio2.into(), p.pins.gpio1.into());
        #[cfg(feature = "cores3-se")]
        let (nfc_sda, nfc_scl) = (p.pins.gpio17.into(), p.pins.gpio18.into());
        // 免許証の読み取りは UiCommand::License で UI へ届け、点呼確認画面へ直行させる
        alc_hub_drivers::nfc::start(
            nfc_sda,
            nfc_scl,
            Arc::clone(&status),
            speaker_tx,
            tx.clone(),
        )?;
    }
    // Base LAN PoE v1.2 (W5500): CS=G9 / RST=G7 / INT=G14 未使用。
    // 旧 LAN Module 13.2 は CS ジャンパが G1/G13 の二択で、G13 が内蔵スピーカーの
    // I2S DOUT と固定で競合していた。Base LAN PoE v1.2 は CS が G9 に出るため
    // スピーカーと排他にする必要がない (plan/cores3-hub-consolidation.md「次期構成」)
    #[cfg(feature = "lan")]
    lan::start(
        spi,
        p.pins.gpio9.into(),
        p.pins.gpio7.into(),
        sysloop,
        Arc::clone(&status),
    )?;
    // NTP: ネットワーク接続後に時刻同期し、測定ログを日本時間で記録する。
    // EspSntp は drop すると同期が止まるため、UI ループ (戻らない) の間
    // 生かし続ける。
    let _sntp = ntp::start()?;
    // NT-100B / NBP-1BLE 読み取り。測定値は meas_tx で recorder へ送る。
    // 接続開始/終了は tx で UI へ通知 (点呼画面の取得中スピナー)。
    // Wi-Fi 接続/Improv セッション中は BLE スキャンを一時停止する (RadioCoex)
    // UI も Measurement を送る (点呼開始時の免許証 kind=license、#125)
    let ui_meas_tx = meas_tx.clone();
    ble::start(Arc::clone(&status), meas_tx, tx, coex, pair_flag)?;

    // 全サービスの起動に成功 = 正常起動として rollback を確定解除する
    // (OTA 直後の初回起動でここまで来られなければ、ブートローダが次の
    // リセットで旧スロットへ自動で戻す。ota.rs 参照)
    alc_hub_drivers::ota::mark_boot_valid();

    // UI ループ (メインタスクを占有, 戻らない)
    ui::run(display, i2c, rx, status, rotation, boot_id, ui_meas_tx)
}
