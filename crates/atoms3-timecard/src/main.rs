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
//! ピンが競合しない根拠 (M5 公式、2026-09-05 に確認):
//!
//! - 内蔵オーディオ (ES8311/NS4150B) は G45/G0/G48/G4/G3/G17/G11/G18、
//!   IR_TX は G47、本体ボタンは G41 — **どれも底面バスにも Grove にも出ない**
//!   (`docs.m5stack.com/en/core/Atom_EchoS3R` のピンマップ)
//! - 底面バスは **G5 / G6 / G7 / G8 / G38 / G39**。VoiceS3R は本体基板が
//!   AtomS3R (`Sch_M5_AtomS3R_v0.4.1.pdf`) + 音声ドーターボード
//!   (`Sch_M5_AtomEchoS3R_Audio_v1.0`) の 2 枚構成で、音声側の回路図には
//!   GPIO ネットが 1 本も出てこない。底面バスの一覧は AtomS3R 公式ドキュメント
//!   (`docs.m5stack.com/en/core/AtomS3R` の "Bottom GPIO")
//! - 内蔵 I2C は G0/G45 なので、Grove (G1/G2) の Unit NFC と競合しない
//!
//! # ★ 本機に RGB LED は無い
//!
//! **Atom VoiceS3R は RGB LED を積んでいない。** 旧 Atom Voice (ESP32) の
//! WS2812 は無くなっている — M5 公式 SKU ページ (C126-Echo) の比較表が
//! "Atom VoiceS3R has no RGB LED" と明記しており、M5Unified の RGBLED ピン表
//! (`M5Unified.cpp` の `_pin_table_other0`) にも `board_M5AtomVoiceS3R` の
//! 行が無い (AtomS3 Lite は G35 で載っている)。
//!
//! したがって #151 で `led.rs` ごと削除した。**検知の可否は serial ログ
//! (`EVT TIMECARD` / `EVT NFC_MULTI_CARD`) で見る。**現場向けの可視/可聴
//! フィードバックは ES8311 の音を入れる別 issue で戻す。
//!
//! # 起動順 (変えてはいけない)
//!
//! `crashlog::init` → `Settings::new` → `heap::start` → `console::start` →
//! `ws_uplink::start` → LAN → `ota::mark_boot_valid`。**`crashlog::init` は
//! `heap::start` より前**。配線漏れで `.noinit` のゴミ帳簿に書いて boot loop に
//! なった実害が 2026-07-14 にある。

mod console;

use alc_hub_common::{
    config,
    measurement::UplinkRecord,
    settings::Settings,
    status::{epoch_ms, now_ms, HubStatus, SharedStatus},
};
use alc_hub_core::timecard::{payload_json, CardKind};
use alc_hub_drivers::nfc::NfcEvent;
use alc_hub_drivers::{crashlog, eth_w5500, heap, nfc, ntp, ota, ws_uplink};
use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::{
    delay::FreeRtos,
    peripherals::Peripherals,
    spi::{config::DriverConfig as SpiDriverConfig, Dma, SpiDriver},
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
    let crash = crashlog::init();
    log::info!(
        "alc-hub-atoms3-timecard v{} 起動",
        config::firmware_version_full()
    );

    let p = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;

    // NVS (device credential 等の永続設定)
    let nvs_partition = EspDefaultNvsPartition::take()?;
    let settings = Settings::new(nvs_partition)?;

    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));
    // ヒープ監視 (OOM 捕捉 + low-water 計測) は重いアロケーションより先に登録
    heap::start(Arc::clone(&status))?;

    // ホストコンソール (PING / STATUS / HEAP / OTA / AUTH / WS)
    console::start(Arc::clone(&status), settings.clone())?;

    // cf-alc-recorder への WS 常時接続。打刻イベントはここへ積む。
    // 接続には AUTH SET 済み credential と LAN 接続が必要 (未登録の間は
    // 接続しないだけで無害 — 送信キューは NVS 永続なので打刻は失わない)
    let (ws_meas_tx, ws_meas_rx) = mpsc::channel();
    // 本機は画面を持たないので UiCommand の受け側は捨てる。_ui_rx は main が
    // 保持し続ける (drop すると ws_uplink スレッドが channel 切断で終了する)
    let (ui_tx, _ui_rx) = mpsc::channel();
    // boot_id は NTP 未同期で記録した測定の時刻補正に使う (ws_uplink.rs)。
    // **打刻は時刻が命**なので、この補正は本機でこそ効く
    let boot_id = settings.next_boot_id();
    ws_uplink::start(
        ws_meas_rx,
        ui_tx,
        Arc::clone(&status),
        settings.clone(),
        boot_id,
    )?;

    // 前回がクラッシュ由来なら panic 前ログを kind=crash_log で送信キューへ
    if let Some(snap) = &crash {
        crashlog::report(snap, &ws_meas_tx, &status);
    }

    // W5500 (Atomic PoE Base): SCLK=G5 / MISO=G7 / MOSI=G8 / CS=G6。
    // DMA 必須 — 無効だと SPI 転送が 64 バイト上限になり、Ethernet フレーム
    // (最大 ~1.5KB) の read/write が "spi transmit failed" で全滅する
    let spi = SpiDriver::new(
        p.spi2,
        p.pins.gpio5,
        p.pins.gpio8,
        Some(p.pins.gpio7),
        &SpiDriverConfig::new().dma(Dma::Auto(4096)),
    )?;
    // leak して 'static 参照で渡す (eth_w5500::start の doc コメント参照)
    let spi: &'static SpiDriver<'static> = Box::leak(Box::new(spi));
    eth_w5500::start(spi, p.pins.gpio6.into(), None, sysloop, Arc::clone(&status))?;

    // Unit NFC (ST25R3916): Grove Port A (SDA=G2 / SCL=G1)。読み取りループは
    // hub-drivers/src/nfc.rs (CoreS3 と共有)。**ここに NFC のコードを書かない**
    nfc::start(
        I2C_PORT_NFC,
        p.pins.gpio2.into(),
        p.pins.gpio1.into(),
        Arc::clone(&status),
        move |e: &NfcEvent| on_card(e, &ws_meas_tx),
    )?;

    // 起動完了 = OTA rollback 解除 (CoreS3 と同じ安全装置、ota.rs 参照)
    ota::mark_boot_valid();

    // SNTP。**打刻端末では必須** — 起動しないとシステム時刻が 1970 のままで、
    // 打刻の `recorded_at_ms` が 1970 起点で送られる (範囲内なので DB 側で NULL
    // にもならず、静かに 55 年ずれた打刻が入る)。`ws_uplink` の
    // `should_wait_for_clock` は 60 秒待って諦め、`fix_unsynced_times` は
    // 「あとで同期したら補正する」仕組みなので、同期が来なければ永久に発火しない。
    // **ここで即起動してはいけない** — 理由は ntp::start_when_online の doc
    let mut sntp = None;

    // メインループ: SNTP の遅延起動だけ (ホスト向けイベントは
    // eth_w5500 / heap / ws_uplink の各スレッドが出す)
    loop {
        FreeRtos::delay_ms(100);
        ntp::start_when_online(
            &mut sntp,
            status.lock().map(|s| !s.lan_ip.is_empty()).unwrap_or(false),
        );
    }
}

/// カードを 1 枚読めたときの処理: 打刻イベントを送信キューへ積む。
///
/// **`card_id` は生値のまま**送る (接頭辞を付けると punch のカード照合が
/// 必ず外れる — alc_hub_core::timecard の doc 参照)。`session_id` は
/// 点呼ではないので付けない。
fn on_card(event: &NfcEvent, ws_tx: &mpsc::Sender<UplinkRecord>) {
    let (card_id, kind) = match event {
        NfcEvent::Felica { idm } => (idm.clone(), CardKind::FelicaIdm),
        NfcEvent::NfcaUid { uid } => (uid.clone(), CardKind::NfcaUid),
        // 免許証は「交付日 8 桁 + 有効期限 8 桁」= alc-app タブレットが使う
        // employees.nfc_id と同じキー。punch はカード未登録なら
        // employees.nfc_id へフォールバックするので、この 16 桁で当たる
        NfcEvent::License { issue, expiry } => {
            let card = alc_hub_core::tenko_prompt::LicenseCard {
                issue: issue.clone(),
                expiry: expiry.clone(),
            };
            match card.nfc_id() {
                Some(id) => (id, CardKind::License),
                None => {
                    // 日付が 8 桁数字でなければキーにできない (壊れた読み取り)
                    log::warn!("timecard: 免許証の日付が想定外 issue={issue} expiry={expiry}");
                    return;
                }
            }
        }
        // 電子車検証は人ではないので打刻にしない (検知ログだけ nfc.rs が出す)
        NfcEvent::CarInspection { .. } => return,
        // **2 枚見えたら、どちらも打刻しない** (issue #143)。財布に 2 枚
        // 入っていると、どちらの人の打刻か決められないまま 2 人ぶん記録して
        // しまう — 賃金データなので曖昧なら記録しない方を採る。
        //
        // **サーバへは何も送らない** (UplinkRecord を作らない)。`hub_measurements`
        // に新しい kind を足すと rust-alc-api の HUB_MEASUREMENT_KINDS と alc-app
        // の型・一覧まで波及するので、まず端末内 (serial ログ) で完結させ、
        // 運用で「エラーが見えない」と分かってから足す。**本機に LED は無い**
        // ので、現場から見える形にするのは音 (ES8311) を入れる別 issue
        NfcEvent::MultipleCards => {
            println!("EVT NFC_MULTI_CARD");
            return;
        }
        NfcEvent::ReadFailed { .. } => return,
    };

    let record = UplinkRecord {
        kind: "timecard",
        payload: payload_json(&card_id, kind),
        // 打刻時刻。NTP 未同期なら ws_uplink が送信時に稼働時間の差で補正する
        recorded_at_ms: epoch_ms(),
        at_ms: now_ms(),
        session_id: None,
    };
    println!("EVT TIMECARD card_id={card_id} card_kind={}", kind.label());
    if ws_tx.send(record).is_err() {
        // ws_uplink スレッドが死んでいる = 送信不能
        log::error!("timecard: 送信キューへ積めなかった (ws_uplink が停止)");
    }
}
