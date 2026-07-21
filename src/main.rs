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
    // LAN Module 13.2 の W5500 (CS=G13、lan.rs) が共有する。G35 は LCD の
    // DC と二役 (display.rs SharedDcInterface 参照)。DMA 必須 — 無効だと
    // Ethernet フレーム転送が 64 バイト上限で全滅する (atoms3-print の実機知見)
    board::power::init(&mut i2c)?;
    let rotation = settings.rotation();
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

    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));
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
    ws_uplink::start(ws_rx, tx.clone(), Arc::clone(&status), settings.clone())?;

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
    rs232::start(
        p.uart1,
        p.pins.gpio17,
        p.pins.gpio18,
        Arc::clone(&status),
        meas_tx.clone(),
        tx.clone(),
    )?;
    // Unit NFC (ST25R3916) (issue #84 / #101)。DIN Base Port A (SDA=G2 / SCL=G1)
    // に配線 (AtomS3 ベンチと同一ピン番号、issue #101 の LAN Module 取り外し構成が前提)。
    // I2C1 は C++ 側 (components/nfc_shim → M5HAL) が所有するため p.i2c1 は take しない
    // (I2C0=内部バス G12/G11 電源IC/タッチとは完全に別ポート)。
    // 内蔵スピーカー (I2S DOUT=G13) は LAN Module の CS と同一ピンのため排他 (`lan`
    // feature 参照)。読み取りビープは issue #101 PR2
    #[cfg(feature = "nfc-verify")]
    {
        // I2S (BCK/WS) を先に起動してから AW88298 の I2SEN=1 を書く。逆順だと
        // アンプの内部 PLL がクロック無しの状態で "有効" 遷移を見てロックしない
        // 疑いがある (2026-07-21 実機で無音、fable diag で指摘)
        let mut speaker = alc_hub_drivers::speaker::Speaker::new(
            p.i2s1,
            p.pins.gpio34.into(),
            p.pins.gpio33.into(),
            p.pins.gpio13.into(),
        )?;
        alc_hub_drivers::speaker::init_amp(&mut i2c)?;
        // 起動時セルフテスト音 (issue #101 PR2 実機デバッグ、2026-07-21): カード検知を
        // 待たずに起動直後に鳴らして I2S/AW88298 経路の疎通を切り分ける
        log::info!("speaker: 起動セルフテスト音再生");
        if let Err(e) = speaker.beep(1000.0, 500) {
            log::warn!("speaker: セルフテスト音 失敗: {e:#}");
        }
        alc_hub_drivers::nfc::start(
            p.pins.gpio2.into(),
            p.pins.gpio1.into(),
            Arc::clone(&status),
            speaker,
        )?;
    }
    // LAN Module 13.2 (W5500): CS=G13 (RS232M 併用ジャンパ) / RST=G0 / INT=G10 未使用。
    // G13 は内蔵スピーカーの I2S DOUT と共用のため、LAN Module 取り外し構成
    // (issue #101) では `lan` feature を無効化する (既定 off)
    #[cfg(feature = "lan")]
    lan::start(
        spi,
        p.pins.gpio13.into(),
        p.pins.gpio0.into(),
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
    ble::start(Arc::clone(&status), meas_tx, tx, coex, pair_flag)?;

    // 全サービスの起動に成功 = 正常起動として rollback を確定解除する
    // (OTA 直後の初回起動でここまで来られなければ、ブートローダが次の
    // リセットで旧スロットへ自動で戻す。ota.rs 参照)
    alc_hub_drivers::ota::mark_boot_valid();

    // UI ループ (メインタスクを占有, 戻らない)
    ui::run(display, i2c, rx, status, rotation)
}
