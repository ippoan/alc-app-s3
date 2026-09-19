//! 内蔵 BLE central: ニプロ体温計 NT-100B / 血圧計 NBP-1BLE、Omron 血圧計
//! HEM-6231T / HCR-1901T2 の読み取り。
//!
//! `ippoan/ble-medical-gateway` からの移植:
//! - スキャン → 接続 → notify/indicate 購読の骨組み:
//!   `firmware-rust/src/main.rs` (esp32-nimble PoC, PR #2-#5)
//! - 値のデコード (IEEE 11073 FLOAT/SFLOAT)・JSON 出力:
//!   `src/main.cpp` (Arduino/NimBLE 版, ATOM Lite 実機実績)
//!
//! 送信済み機器の扱い (NT-100B は送信後も電源断まで約 2 分広告を続け、広告
//! 内容は完全に静的で新規測定の有無を判別できない — 実機で確認):
//!
//! - 広告が見えたら常に接続する。一度正常に届いた測定は機器が再送しない
//!   (実機で確認) ため、送信済み機器への再接続はデータなしタイムアウトで
//!   数秒後に切れるだけ。新しい測定はいつでも次の接続で届く
//! - 万一同一測定が再送されても、recorder の機器タイムスタンプ重複排除が
//!   破棄する (画面・ログに二重反映しない)
//! - 接続保持 (パーク) で広告を止める案は不可: ESP32-S3 NimBLE は接続中の
//!   スキャンで広告レポートが届かない既知問題がある (esp-idf issue #15258,
//!   実機でも確認)。接続は受信後すみやかに切断し、スキャンを空ける
//! - 点呼画面のスピナーは「未取得の項目」のみ回す (hub-ui 側) — 空接続で
//!   サークルが回りっぱなしに見えないようにする
//!
//! ホストへの出力は ble-medical-gateway のシリアル JSON 互換
//! (alc-app 側 `useBleGateway` の置き換え想定):
//!
//! ```text
//! {"type":"found","device":"thermometer"}
//! {"type":"connected","device":"blood_pressure"}
//! {"type":"temperature","value":36.5,"unit":"celsius"}
//! {"type":"blood_pressure","systolic":120,"diastolic":80,"pulse":72,"unit":"mmHg"}
//! {"type":"disconnected","device":"thermometer"}
//! {"type":"reset","message":"Scan restarted"}
//! {"type":"error","message":"..."}
//! {"type":"bp_bond","bonded":true}
//! ```
//!
//! `bp_bond` は「血圧計がボンドされているか」の観測値 (Refs #249)。スキャン 1 周
//! ごとに計算し、**変化したときだけ** 1 行出す。同じ値を `HubStatus::bp_bonded`
//! へ写し、`AUTH SIGNBP` の署名対象 (`<nonce>|bp=<1|0>`) に載せる
//! (管理者ログインが使う `AUTH SIGN` は nonce だけに署名し、これを載せない)。

use std::future::{poll_fn, Future};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::Duration;

use alc_hub_core::{
    device::{
        match_device_name, omron_addr_from_name, omron_adv, omron_adv_from_mfg,
        should_remember_bp_bond, BpBondSite, DeviceKind, OmronAdv,
    },
    ieee11073::{parse_blood_pressure, parse_temperature},
};
use anyhow::{Context, Result};
use esp32_nimble::{
    enums::{AuthReq, SecurityIOCap},
    utilities::BleUuid,
    uuid128, BLEAddress, BLEAddressType, BLEAdvertisedData, BLEAdvertisedDevice, BLEClient,
    BLEDevice, BLEScan,
};
use esp_idf_svc::hal::{delay::FreeRtos, task::block_on};
use esp_idf_svc::timer::EspTaskTimerService;

use alc_hub_common::control::PairFlag;
use alc_hub_common::measurement::Measurement;
use alc_hub_common::settings::Settings;
use alc_hub_common::status::{now_ms, SharedStatus};
use alc_hub_common::ui_api::UiCommand;
use alc_hub_core::coex::RadioCoex;

#[cfg(feature = "probe")]
mod probe;

/// on_notify クロージャ (Send + Sync 要求) から使うため Mutex で包む。
/// notify コールバックは nimble_host タスク上で走りスタックが小さいため、
/// ここでは「パースして Measurement を送るだけ」に留める (重い処理は recorder)。
type MeasTx = Arc<Mutex<Sender<Measurement>>>;

const HEALTH_THERMOMETER_SERVICE: u16 = 0x1809;
const BLOOD_PRESSURE_SERVICE: u16 = 0x1810;
const TEMPERATURE_MEASUREMENT: u16 = 0x2A1C;
const BLOOD_PRESSURE_MEASUREMENT: u16 = 0x2A35;
const CURRENT_TIME_SERVICE: u16 = 0x1805;
const CURRENT_TIME: u16 = 0x2A2B;

/// Omron の company id (Bluetooth SIG)。本体の広告のメーカーデータに載る
const OMRON_COMPANY_ID: u16 = 0x020E;

// Omron 独自 service のペアリング (unlock 鍵の登録)。UUID と電文は omblepy
// (https://github.com/userx14/omblepy) の LEGACY_* / writeNewUnlockKey と同じ (Refs #237)
const OMRON_SERVICE: BleUuid = uuid128!("ecbe3980-c9a2-11e1-b1bd-0002a5d5c51b");
/// HCR-1901T2 の Omron 独自 service (16bit)。中の特性 (unlock / RX0) は 128bit の
/// [`OMRON_SERVICE`] と同じ UUID だが、**鍵の電文には一切応答しない** — この系統は
/// bond だけで標準 BLS (0x2A35) を流す (Windows で実測)。この service が在るかで
/// ペアリングの手順を分ける
const OMRON_SERVICE_16: u16 = 0xFE4A;
/// 全 0 アドレスで届いた Omron の本体広告の状態と時刻。名前を持つ scan response と
/// 突き合わせて接続先アドレスを起こすために置く ([`match_target`] を参照)
static OMRON_ZERO_ADV: Mutex<Option<(u64, OmronAdv)>> = Mutex::new(None);
/// 上の記憶の有効期間。scan response は同じ広告への SCAN_REQ の応答なので直後に届く
const OMRON_ZERO_ADV_MS: u64 = 1_000;
/// 全 0 アドレスの Omron 機の名前から起こした MAC。機器ごとに変わらないので一度覚えれば
/// 以降は**状態を持つ本体広告 1 つで**接続先が決まる (2 パケットが揃うのを待たない)。
/// ペアリング待ちは数十秒で終わるので、待ち時間を削るのがそのまま成否に効く
static OMRON_ZERO_MAC: Mutex<Option<[u8; 6]>> = Mutex::new(None);
/// RX[0]。購読すると機器が bond (SMP) を始める
const OMRON_RX0: BleUuid = uuid128!("49123040-aee8-11e1-a74d-0002a5d5c51b");
/// unlock。書き込みへの応答が notify で返る
const OMRON_UNLOCK: BleUuid = uuid128!("b305b680-aee7-11e1-a730-0002a5d5c51b");
/// unlock の電文の先頭: プログラムモードへ入る (続く 16 byte は 0)
const OMRON_OP_PROGRAM_MODE: u8 = 0x02;
/// unlock の電文の先頭: 続く 16 byte を鍵として登録する
const OMRON_OP_SET_KEY: u8 = 0x00;
/// 応答 notify の先頭 2 byte (成功)
const OMRON_ACK_PROGRAM_MODE: [u8; 2] = [0x82, 0x00];
const OMRON_ACK_SET_KEY: [u8; 2] = [0x80, 0x00];
/// プログラムモードの要求を繰り返す回数と、1 回ごとの応答待ち
const OMRON_PROGRAM_MODE_TRIES: u32 = 10;
const OMRON_PROGRAM_MODE_WAIT_MS: u64 = 1_000;
const OMRON_SET_KEY_WAIT_MS: u64 = 3_000;
/// Omron 機への secure_connection の上限。保存済みの鍵で暗号化を始めて機器が応じないと、
/// ENC_CHANGE も切断も来ずに止まる (S3R で実測)
const OMRON_SECURE_TIMEOUT_MS: u64 = 10_000;
/// early start のあと、この時間暗号化されなければ暗号化を始め直す
const OMRON_ENC_RESTART_MS: u64 = 3_000;
/// early start 後のサービス探索で、見つかった数がこの時間変わらなければ完了とみなす
const OMRON_SERVICES_SETTLE_MS: u64 = 400;
/// 鍵登録の後始末 (CCCD を 0 に) のあと、切断までに置く時間 (Linux の手順と同じ)
const OMRON_PAIR_LINGER_MS: u64 = 3_000;

// スキャンを連続化して隙間を無くす。ニプロ機器は測定後の短時間しか広告
// しないため、隙間があると取り逃す。5 秒ごとに coex/再ペアリング要求を確認し、
// 機器発見時はコールバックが Some を返して即座にスキャンを抜ける。
const SCAN_DURATION_MS: i32 = 5_000;
const SCAN_COOLDOWN_MS: u32 = 0;
const MIN_RSSI: i8 = -80;
const CONNECT_RETRIES: u32 = 3;
/// データ受信後、続報が「途切れた」とみなして切断・転送するまでの静穏時間。
/// 体温計は 1 件のみなので短く、血圧計は過去分ダンプの間隔を見込んで長めに取る
fn data_quiet_ms(kind: DeviceKind) -> u64 {
    match kind {
        DeviceKind::Thermometer => 300,
        DeviceKind::BloodPressure => 1_000,
    }
}

/// 接続後この時間データが来なければ諦めて切断・再スキャンする。
/// 無い場合、無言の機器に繋がると supervision timeout (~99秒) まで BLE ループ
/// 全体がブロックされ、体温も血圧も取れなくなる (実機ログで確認)。
/// データがある機器は購読後 1 秒以内に送ってくる実績のため短めでよい
const DATA_WAIT_TIMEOUT_MS: u64 = 3_000;

/// データなしで終わった機器への再接続を控える時間。送信済み機器へ数秒周期で
/// 接続し続けると機器側がふさがり、測り直しのトリガーや新データの引き渡しが
/// 遅れる (実機で確認)。短いバックオフで機器に空き時間を作る
const EMPTY_BACKOFF_MS: u64 = 10_000;

/// `PAIR` (Pages の「血圧計を再ペアリング」) で開くペアリング受付の長さ。
/// Omron 機の `-P-` 点滅は 1〜2 分で切れるので、それを覆う程度に取る
const PAIR_ARM_MS: u64 = 120_000;
/// ペアリングを終えた Omron 機へペアリング接続を控える時間。鍵登録の直後も
/// ペアリング待ちの広告が数秒残り、そこへ再接続すると機器に切られる (S3R で実測)
const OMRON_PAIRED_BACKOFF_MS: u64 = 30_000;
/// Omron 機の送信接続で、機器が自分で切断するのを待つ上限。機器は自分で切断するまで
/// つながっていないと記録を送信済みにせず、S3R から切った回は次の接続で同じ記録を再送した
/// (Linux で記録が届いた接続は、indication のあと約 8 秒で機器が切断していた)
const OMRON_SESSION_TIMEOUT_MS: u64 = 20_000;

fn service_uuid(kind: DeviceKind) -> BleUuid {
    match kind {
        DeviceKind::Thermometer => BleUuid::from_uuid16(HEALTH_THERMOMETER_SERVICE),
        DeviceKind::BloodPressure => BleUuid::from_uuid16(BLOOD_PRESSURE_SERVICE),
    }
}

fn measurement_uuid(kind: DeviceKind) -> BleUuid {
    match kind {
        DeviceKind::Thermometer => BleUuid::from_uuid16(TEMPERATURE_MEASUREMENT),
        DeviceKind::BloodPressure => BleUuid::from_uuid16(BLOOD_PRESSURE_MEASUREMENT),
    }
}

pub fn start(
    status: SharedStatus,
    meas_tx: Sender<Measurement>,
    ui_tx: Sender<UiCommand>,
    coex: Arc<RadioCoex>,
    pair_flag: PairFlag,
    settings: Settings,
) -> Result<()> {
    let meas_tx: MeasTx = Arc::new(Mutex::new(meas_tx));
    // 血圧計を観測する経路が在ることを記録する (Refs #269)。これが false の機
    // (BLE を積まない alarm / print、AtomS3 Lite build、`OMRON BP OFF` で
    // `start` を呼ばない timecard / bp-station) は `bp_bonded` が永久に既定値の
    // false のままで、それが**正しい観測結果** = 「血圧計なし」。読了ゲート
    // (`alc_hub_core::device::bp_report`) はその区別にこの旗を使う
    if let Ok(mut st) = status.lock() {
        st.ble_running = true;
    }
    alc_hub_common::task::name_next(c"ble");
    std::thread::Builder::new()
        .name("ble".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            if let Err(e) = block_on(task(status, meas_tx, ui_tx, coex, pair_flag, settings)) {
                log::error!("ble: タスク異常終了: {e:?}");
                println!("{{\"type\":\"error\",\"message\":\"BLE task terminated\"}}");
            }
        })?;
    Ok(())
}

async fn task(
    status: SharedStatus,
    meas_tx: MeasTx,
    ui_tx: Sender<UiCommand>,
    coex: Arc<RadioCoex>,
    pair_flag: PairFlag,
    settings: Settings,
) -> Result<()> {
    let device = BLEDevice::take();

    // Arduino 版と同等の Just Works ボンディング設定
    device
        .security()
        .set_auth(AuthReq::Bond)
        .set_io_cap(SecurityIOCap::NoInputNoOutput);

    let mut scan = BLEScan::new();
    // データなしで終わった機器 (アドレス, 終了時刻)。EMPTY_BACKOFF_MS の間は
    // 再接続せず、機器を空けて測り直しを受け付けやすくする
    let mut empty_backoff: Vec<(BLEAddress, u64)> = Vec::new();
    // ペアリングを終えた Omron 機 (アドレス, 終了時刻)。OMRON_PAIRED_BACKOFF_MS の間は
    // ペアリング待ちの広告に接続しない (送信広告には接続する)
    let mut paired_backoff: Vec<(BLEAddress, u64)> = Vec::new();
    // 血圧計としてボンドした機器のアドレス (NVS `bp_bond` の写し。Refs #249)。
    // **真偽ではなくアドレスだけ**を持ち、現在値はボンド一覧と突き合わせて毎回決める。
    // 手元に持つのは、スキャン 1 周ごとに NVS を読みに行かないため
    let mut bp_bond_rec = settings.bp_bond_addr();
    // ホストへ出した直近の値 (変化したときだけ 1 行出す)。None = 未出力
    let mut last_bp: Option<bool> = None;
    // 「未ボンド」の内訳の直近値 `(記録が在るか, ボンド一覧に居るか)` (Refs #252)。
    // これも変化したときだけ出す — スキャンは 1 周ごとに回るため
    let mut last_bp_diag: Option<(bool, bool)> = None;
    // ペアリング受付の期限 [ms] (0 = 受け付けていない)。**`PAIR` を押した間だけ**
    // ペアリング待ちの広告に接続する。押していない間は `-P-` を見ても繋がない —
    // 見つけ次第ペアリングすると、そばで誰かが `-P-` にしただけで機器のボンド枠を
    // 黙って奪い、元の相手 (スマホ等) との組が切れる
    let mut pair_until: u64 = 0;
    loop {
        // 再ペアリング要求: 保存済みボンドを全消去する。壊れた/古いボンドが
        // 血圧計の暗号化接続を妨げている場合の復旧手段 (Pages のペアリングボタン)
        if pair_flag.swap(false, Ordering::SeqCst) {
            match device.delete_all_bonds() {
                Ok(()) => {
                    log::info!("ble: 全ボンドを消去 (再ペアリング)");
                    if let Ok(mut st) = status.lock() {
                        st.push_event(now_ms(), "ペアリング情報を消去");
                    }
                    alc_hub_common::evtlog::emit("EVT PAIR_CLEARED");
                    // ボンドが無くなった = 血圧計の記録も残さない (Refs #249)
                    match settings.clear_bp_bond_addr() {
                        Ok(()) => bp_bond_rec = None,
                        Err(e) => log::warn!("ble: bp_bond 消去失敗: {e:?}"),
                    }
                }
                Err(e) => {
                    log::warn!("ble: ボンド消去失敗: {e:?}");
                    alc_hub_common::evtlog::emit("EVT PAIR_ERR ボンド消去に失敗");
                }
            }
            // 消去の成否に関わらず受付を開く (要求されたのはペアリングのやり直し)
            pair_until = now_ms() + PAIR_ARM_MS;
            alc_hub_common::evtlog::emit(&format!("EVT PAIR_ARMED {}", PAIR_ARM_MS / 1_000));
        }
        // 受付時間が切れた: 1 回だけ知らせる (Pages が結果表示を終えられるように)
        if pair_until != 0 && now_ms() >= pair_until {
            pair_until = 0;
            alc_hub_common::evtlog::emit("EVT PAIR_TIMEOUT");
        }
        let pair_open = pair_until != 0;

        // Wi-Fi の接続/スキャン中 + Improv セッション中は BLE スキャンを
        // 止め、コエグジストの電波取り合いで Wi-Fi 側が失敗しないようにする
        // OTA 中は scan を止めて内部RAM を譲る (Refs #116)。coex の pause と
        // 同じ止め方で、OTA が終われば (成功なら再起動で) 自然に再開する。
        while coex.ble_should_pause(now_ms())
            || status.lock().map(|st| st.ota_active).unwrap_or(false)
        {
            FreeRtos::delay_ms(200);
        }

        // バックオフ期限切れの機器を解放
        empty_backoff.retain(|(_, at)| now_ms().saturating_sub(*at) < EMPTY_BACKOFF_MS);
        paired_backoff.retain(|(_, at)| now_ms().saturating_sub(*at) < OMRON_PAIRED_BACKOFF_MS);

        // Omron 血圧計を拾うか (NVS、既定 OFF)。ループ 1 周に 1 回だけ読む —
        // スキャンの callback は広告 1 件ごとに呼ばれるため、その中で lock しない
        let omron_enabled = status.lock().map(|st| st.omron_bp).unwrap_or(false);

        // 血圧計がボンドされているか (`AUTH SIGNBP` の署名対象に載る、Refs #249)。
        // 真偽を NVS には持たず、記録したアドレスがボンド一覧にまだ居るかで毎回決める
        let (bp_rec, bp) = bp_bond_parts(bp_bond_rec);
        if let Ok(mut st) = status.lock() {
            st.bp_bonded = bp;
            st.bp_read = true;
        }
        if last_bp != Some(bp) {
            last_bp = Some(bp);
            println!("{{\"type\":\"bp_bond\",\"bonded\":{bp}}}");
        }
        // 「未ボンド」に見えるときの内訳 (Refs #252)。記録が無い (`rec=0`) のか、
        // 記録はあるが NimBLE のボンド一覧に居ない (`rec=1 listed=0`) のかで原因が
        // 違う — 前者は記録漏れ (測定がまだ一度も届いていない)、後者は機器が
        // ボンドを張っていない。アドレスは端末の識別子になりうるので出さない
        // ([`remember_bp_bond`] と同じ方針) — 真偽だけにする
        if !bp && last_bp_diag != Some((bp_rec, bp)) {
            alc_hub_common::evtlog::emit(&format!(
                "EVT BP_BOND none rec={} listed={}",
                u8::from(bp_rec),
                u8::from(bp)
            ));
        }
        last_bp_diag = Some((bp_rec, bp));

        // ニプロ機器は測定時にアドバタイズを開始するため、短いスキャンを
        // 繰り返して発見次第すぐ接続する (Arduino 版 loop() と同じ運用)。
        // 送信済み機器の広告にも接続する — 一度届いた測定は再送されず
        // 数秒の空接続で終わり、万一の再送は recorder の重複排除が破棄する
        let target = scan
            .active_scan(true)
            .interval(100)
            .window(99)
            .start(device, SCAN_DURATION_MS, |dev, data| {
                #[cfg(feature = "probe")]
                probe::log_adv(dev, &data);
                let target = match_target(dev, &data);
                // 直近の接続がデータなしだった機器はバックオフ中 — 接続しない。
                // ただしペアリング待ちは通す: HCR-1901T2 は同じ機器の scan response が
                // 名前だけで「送信」に見え、未ボンドで弾かれて backoff に入る。その間
                // ユーザーが -P- にした本体広告まで捨てると、ペアリングが永久に始まらない
                if !matches!(target, Some((_, _, Some(OmronAdv::Pairing))))
                    && target.is_some_and(|(a, _, _)| empty_backoff.iter().any(|(b, _)| *b == a))
                {
                    return None;
                }
                match target {
                    Some((_, _, Some(_))) if !omron_enabled => None,
                    // 受付が開いていないペアリング待ちの広告は見送る (送信広告は拾う)
                    Some((_, _, Some(OmronAdv::Pairing))) if !pair_open => None,
                    Some((addr, _, Some(OmronAdv::Pairing)))
                        if paired_backoff.iter().any(|(a, _)| *a == addr) =>
                    {
                        None
                    }
                    target => target,
                }
            })
            .await
            .context("BLE スキャン失敗")?;

        let Some((addr, kind, omron)) = target else {
            FreeRtos::delay_ms(SCAN_COOLDOWN_MS);
            continue;
        };

        // bond の無い Omron 機の送信広告には接続しない。secure_connection がその場で新しく
        // ペアリングしてしまい、機器はその bond には記録を送らない (S3R で実測)
        if omron == Some(OmronAdv::Transfer) {
            if matches!(omron_find_bond(&addr), Ok(Some(_))) {
                // bond 済みの Omron 機を見た = 血圧計がボンドされている観測点。
                // 記録してよいかの判定は hub-core の述語 1 本が持つ (Refs #266)
                if should_remember_bp_bond(BpBondSite::OmronBondSeen) {
                    remember_bp_bond(&settings, &addr, &mut bp_bond_rec);
                }
            } else {
                alc_hub_common::evtlog::emit("EVT OMRON_ENC nobond");
                empty_backoff.push((addr, now_ms()));
                continue;
            }
        }

        println!("{{\"type\":\"found\",\"device\":\"{}\"}}", kind.json_name());
        // 非 Omron の血圧計 (ニプロ NBP-1 等) は evtlog を 1 行も出しておらず、
        // 実機で測ってもハブが血圧計を見つけたのかどうかログから判定できなかった
        // (Refs #252)。Omron 経路は EVT OMRON_* が既に出すので二重に出さない
        let bp_nonomron = kind == DeviceKind::BloodPressure && omron.is_none();
        if bp_nonomron {
            alc_hub_common::evtlog::emit("EVT BP_CONN try");
        }
        // 接続開始を UI へ通知 → 点呼画面のラベル横に取得中スピナーを表示
        let _ = ui_tx.send(UiCommand::BleAcquiring { device: kind });

        let mut client = device.new_client();
        let result = handle_device(&mut client, addr, kind, omron, &status, &meas_tx).await;
        // 測定を受け取れたか / 受け取れずに終わったか (Refs #252)
        if bp_nonomron {
            alc_hub_common::evtlog::emit(match &result {
                Ok(true) => "EVT BP_RX ok",
                Ok(false) => "EVT BP_RX none",
                Err(_) => "EVT BP_RX err",
            });
        }
        // 血圧の特性を実際に読めた経路だけ「血圧計としてボンド」に載せる (Refs #252)。
        // 非 Omron 機はここまで記録を書く経路が無く、NimBLE のボンド一覧に居ても
        // bp_bonded が永久に false のままだった。判定は hub-core の述語 1 本が持つ
        if should_remember_bp_bond(BpBondSite::ConnectionFinished {
            kind,
            omron,
            got_data: matches!(result, Ok(true)),
        }) {
            remember_bp_bond(&settings, &addr, &mut bp_bond_rec);
        }
        // ペアリングは 1 回やり切ったら受付を閉じる。失敗は開けたままにして、
        // 機器の `-P-` が続く間の再試行を受け付ける
        if omron == Some(OmronAdv::Pairing) {
            if result.is_ok() {
                pair_until = 0;
                // ペアリング成立。match_target が血圧計と確定させた機なので、
                // 「血圧計としてボンドした」アドレスとして記録する (Refs #249)。
                // ★ 記録は EVT PAIR_OK と同じブロックで行う — 別条件に分けると
                // 「成功と表示したのに BP=0」が作れてしまう (測定 0 件で機器側から
                // 切断された回に現場が止まった。Refs #266)
                if should_remember_bp_bond(BpBondSite::OmronPaired) {
                    remember_bp_bond(&settings, &addr, &mut bp_bond_rec);
                }
                alc_hub_common::evtlog::emit("EVT PAIR_OK");
            } else {
                alc_hub_common::evtlog::emit("EVT PAIR_ERR 接続に失敗");
            }
        }
        match result {
            // データなし: しばらくこの機器への再接続を控える (機器を空ける)。
            // 接続失敗 (Err) はバックオフしない — 新規測定の広告での一時的な
            // 接続失敗もあり、その場合は即リトライで拾いたい。Omron の送信接続は
            // 失敗でも控える (送信広告に繰り返し接続して機器をふさがない)
            Ok(false) => empty_backoff.push((addr, now_ms())),
            // ボンド記録は EVT PAIR_OK と同じブロックで済んでいる (Refs #266)。
            // ここに残るのは「同じ機にすぐ再接続しない」ためのバックオフだけ
            Ok(true) if omron == Some(OmronAdv::Pairing) => {
                paired_backoff.push((addr, now_ms()));
            }
            Ok(true) => {}
            Err(e) => {
                if omron == Some(OmronAdv::Transfer) {
                    empty_backoff.push((addr, now_ms()));
                }
                log::warn!("ble: {} 処理失敗: {e:?}", kind.json_name());
                println!(
                    "{{\"type\":\"error\",\"message\":\"{}: connection failed\"}}",
                    kind.json_name()
                );
            }
        }
        drop(client);

        if let Ok(mut st) = status.lock() {
            st.ble_connected = false;
            st.ble_device.clear();
        }
        // 取得シーケンス終了 (測定値は転送済み or 失敗) → スピナー消去
        let _ = ui_tx.send(UiCommand::BleIdle);
        println!("{{\"type\":\"reset\",\"message\":\"Scan restarted\"}}");
    }
}

/// 広告が対象サービス (体温計/血圧計) を含み RSSI が閾値以上なら種別を返す。
/// Arduino 版と同様、標準サービス UUID に加えてデバイス名でも判定する
/// (ニプロ機器が独自名を使う場合の対策)。Omron 機器は service UUID を広告しないので
/// 名前で当たり、広告の状態 (ペアリング待ち / 送信) も一緒に返す
fn match_target(
    dev: &BLEAdvertisedDevice,
    data: &BLEAdvertisedData<&[u8]>,
) -> Option<(BLEAddress, DeviceKind, Option<OmronAdv>)> {
    if dev.rssi() < MIN_RSSI {
        return None;
    }

    // HCR-1901T2 のペアリング待ちの広告は S3R の NimBLE では**アドレスが全 0** で届く
    // (同時刻に Windows は実アドレス F8:B3:… を受けているので、機器は実アドレスで
    // 広告している)。全 0 では接続できないので、名前 (scan response) に入っている MAC
    // から起こす。状態 (ペアリング待ち / 送信) は名前の無い本体広告のメーカーデータに
    // しかないため、直前の本体広告の状態を覚えて突き合わせる
    let zero_addr = dev.addr().as_le_bytes() == [0u8; 6];

    // Omron 機の本体の広告は名前が無く 0x1810 を広告し、名前は別パケットの scan response
    // にだけ載る (S3R の PROBE ADV で実測)。本体の広告を 0x1810 で当てると Omron と
    // 分からずニプロ経路で接続するので無視し、scan response の名前で判定させる
    if let Some(mfg) = data
        .manufacture_data()
        .filter(|m| m.company_identifier == OMRON_COMPANY_ID)
    {
        // HCR-1901T2 の状態はメーカーデータの flag だけが持つ。名前 (scan response) は
        // ペアリング待ちでも `BLESmart_` のままで大文字小文字判定が効かない
        let adv = match omron_adv_from_mfg(mfg.payload) {
            Some(adv) => adv,
            None => omron_adv(&String::from_utf8_lossy(data.name()?))?,
        };
        if zero_addr {
            // 本体広告は名前を載せない。MAC を既に覚えていればそのまま接続先にする
            if let Some(mac) = OMRON_ZERO_MAC.lock().ok().and_then(|mac| *mac) {
                return Some((
                    BLEAddress::from_be_bytes(mac, BLEAddressType::Random),
                    DeviceKind::BloodPressure,
                    Some(adv),
                ));
            }
            // まだ知らないときは状態だけ覚えて、直後に来る scan response で拾い直す
            if let Ok(mut slot) = OMRON_ZERO_ADV.lock() {
                *slot = Some((now_ms(), adv));
            }
            return None;
        }
        return Some((dev.addr(), DeviceKind::BloodPressure, Some(adv)));
    }

    if zero_addr {
        // 名前だけの scan response。名前の MAC を覚えて以降の本体広告で使い、この場では
        // 直前の本体広告の状態と結び付いたときだけ接続先にする
        // (機器は public ではなく random で広告している)
        let mac = omron_addr_from_name(&String::from_utf8_lossy(data.name()?))?;
        if let Ok(mut slot) = OMRON_ZERO_MAC.lock() {
            *slot = Some(mac);
        }
        let adv = OMRON_ZERO_ADV.lock().ok().and_then(|slot| {
            slot.filter(|(at, _)| now_ms().saturating_sub(*at) < OMRON_ZERO_ADV_MS)
                .map(|(_, adv)| adv)
        })?;
        return Some((
            BLEAddress::from_be_bytes(mac, BLEAddressType::Random),
            DeviceKind::BloodPressure,
            Some(adv),
        ));
    }

    if data.is_advertising_service(&BleUuid::from_uuid16(HEALTH_THERMOMETER_SERVICE)) {
        return Some((dev.addr(), DeviceKind::Thermometer, None));
    }
    if data.is_advertising_service(&BleUuid::from_uuid16(BLOOD_PRESSURE_SERVICE)) {
        return Some((dev.addr(), DeviceKind::BloodPressure, None));
    }

    if let Some(name) = data.name() {
        // name() は生バイト列 (&[u8]) を返す
        let name = String::from_utf8_lossy(name);
        return match_device_name(&name).map(|kind| (dev.addr(), kind, omron_adv(&name)));
    }
    None
}

/// 接続 → 購読 → 測定値の受信 → 切断。データを受信したかを返す
/// (false なら呼び出し側が短いバックオフを掛けて機器を空ける)
async fn handle_device(
    client: &mut BLEClient,
    addr: BLEAddress,
    kind: DeviceKind,
    omron: Option<OmronAdv>,
    status: &SharedStatus,
    meas_tx: &MeasTx,
) -> Result<bool> {
    let disconnected = Arc::new(Disconnected::default());
    {
        let disconnected = Arc::clone(&disconnected);
        client.on_disconnect(move |_| disconnected.set());
    }
    // Omron 機には送らない (Linux で記録が届いた回は接続パラメータを変えていない)
    let tune_conn_params = omron.is_none();
    client.on_connect(move |client| {
        if !tune_conn_params {
            return;
        }
        // Arduino 版と同様、接続直後に conn params を更新する
        if let Err(e) = client.update_conn_params(120, 120, 0, 60) {
            log::warn!("ble: update_conn_params 失敗: {e:?}");
        }
    });

    // ペアリング待ちの Omron 機は、保存済みのその機器の bond を先に消す。残っていると
    // secure_connection が新しいペアリングにならず古い鍵での暗号化で終わる (Linux の成功例は
    // 毎回消していた)。delete_bond (ble_gap_unpair) は接続中だと切断するので接続の前に行う
    if omron == Some(OmronAdv::Pairing) {
        omron_unbond(&addr);
    }

    // Arduino 版と同様に最大 3 回リトライ
    let mut attempt = 0;
    // Omron の送信広告への接続では、接続ができた瞬間に暗号化を始める
    let mut early_enc: Option<EarlyEnc> = None;
    loop {
        attempt += 1;
        disconnected.clear();
        // connect は exchange MTU の完了を待つが、その前に切られると esp32-nimble が
        // 完了を知らせず await が戻らない (S3R で 2 分停止)。切断でも抜ける形で待つ
        let connect = connect_encrypting(
            client.connect(&addr),
            addr,
            omron == Some(OmronAdv::Transfer),
        );
        let res = disconnected.until(connect).await.map(|(res, early)| {
            early_enc = early;
            res
        });
        match res {
            Ok(Ok(())) => break,
            Ok(Err(e)) if attempt < CONNECT_RETRIES => {
                log::warn!("ble: 接続リトライ {attempt}/{CONNECT_RETRIES}: {e:?}");
                FreeRtos::delay_ms(500);
            }
            Ok(Err(e)) => return Err(e).context("接続失敗 (リトライ上限)"),
            Err(e) if attempt < CONNECT_RETRIES => {
                log::warn!("ble: 接続リトライ {attempt}/{CONNECT_RETRIES}: {e:?}");
                FreeRtos::delay_ms(500);
            }
            Err(e) => return Err(e).context("接続失敗 (リトライ上限)"),
        }
    }

    match omron {
        Some(OmronAdv::Pairing) => match omron_pair(client, &disconnected).await {
            // HCR-1901T2 (bls): **同じ接続のまま**購読へ進む。bond だけで切ると機器は
            // 登録が終わったと見なさず -P- が消えない (実機で確認)
            Ok(true) => {}
            // HEM-6231T (legacy): 鍵登録で完結。切って送信広告を待つ。
            // ペアリングはデータなしでも Ok(true) を返す — Ok(false) だと呼び出し側が
            // EMPTY_BACKOFF_MS の間この機器へ接続しなくなり、ペアリング直後に機器が出す
            // 送信広告 (未送信の記録) を取り逃す
            Ok(false) => {
                let _ = client.disconnect();
                return Ok(true);
            }
            Err(e) => {
                let _ = client.disconnect();
                return Err(e);
            }
        },
        Some(OmronAdv::Transfer) => {
            // Omron 機はニプロ機と違い、保存済み bond で先に暗号化しないと 0x2A35 を
            // 送らない (Linux で実測)。時刻 (0x2A2B) は書かない — 書くと記録が独自形式の
            // 別 characteristic に移る
            // early start が通っていれば secure_connection は呼ばない: 暗号化が済んだリンクで
            // 呼ぶと NimBLE は 2 回目の暗号化を始める (EALREADY になるのは手順の進行中だけ)
            let mut early = early_enc.filter(|e| e.rc == 0);
            let res = match early.as_mut() {
                Some(e) if e.encrypted => Ok(Ok(())),
                Some(e) => {
                    wait_encrypted(client, &disconnected, OMRON_SECURE_TIMEOUT_MS, e).map(Ok)
                }
                None => {
                    disconnected
                        .until_timeout(client.secure_connection(), OMRON_SECURE_TIMEOUT_MS)
                        .await
                }
            };
            // early start を始め直した回数 (0x3e などで接続が張り直されたとき)
            let retry = early.map_or(0, |e| e.retry);
            match res {
                Ok(Ok(())) => {
                    alc_hub_common::evtlog::emit(&format!("EVT OMRON_ENC ok retry={retry}"))
                }
                Ok(Err(e)) => {
                    log::warn!("ble: Omron 暗号化失敗: {e:?}");
                    alc_hub_common::evtlog::emit(&format!("EVT OMRON_ENC err retry={retry}"));
                }
                // 切断・時間切れ: 暗号化の途中なので打ち切って切断する
                Err(e) => {
                    alc_hub_common::evtlog::emit(&format!("EVT OMRON_ENC err retry={retry}"));
                    let _ = client.disconnect();
                    return Err(e.context("Omron 暗号化の打ち切り"));
                }
            }
            if early.is_some() {
                omron_settle_services(client, &disconnected).await?;
            }
        }
        None => {}
    }

    // ニプロ機では明示的な secure_connection は行わない。血圧計 (NBP-1BLE) は
    // 「接続 → 測定値 indication → 即切断」を非常に短時間で行うため、
    // ペアリングの往復待ちを挟むと購読前に切断され indication を取り逃す
    // (実機ログで確認: secure_connection 成功直後に Remote User Terminated)。
    // Arduino 版と同様に接続後すぐ購読し、暗号化が要求される場合は NimBLE が
    // 購読時 (CCCD 書き込み) に自動ネゴする。ボンドは NVS に永続化される。
    let service = client
        .get_service(service_uuid(kind))
        .await
        .context("サービスが見つからない")?;
    let characteristic = service
        .get_characteristic(measurement_uuid(kind))
        .await
        .context("キャラクタリスティックが見つからない")?;

    // 血圧計は保存済みの過去測定をまとめて送ってくる。セッション中の測定を
    // すべて貯め、最後に「最新 (タイムスタンプ最大) の 1 件」だけを recorder へ
    // 送る。これで過去分が大量に記録されるのを防ぐ。
    let got_data = Arc::new(AtomicBool::new(false));
    // 最終受信時刻 [ms, u32 切り詰め]。静穏時間の判定に使う (wrapping_sub で
    // 差分を取るため 49 日周期の折り返しは問題にならない)。
    // ESP32-S3 (Xtensa) はネイティブ 64bit アトミックが無いため u32
    let last_rx = Arc::new(AtomicU32::new(0));
    let buffer: Arc<Mutex<Vec<(Measurement, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let got_data = Arc::clone(&got_data);
        let last_rx = Arc::clone(&last_rx);
        let buffer = Arc::clone(&buffer);
        // このクロージャは nimble_host タスク上で呼ばれる (スタック小)。
        // パースしてバッファに積むだけに留める — println!/format!/NVS 等の
        // 重い処理は recorder スレッドで行う (以前ここで直接やって血圧受信時に
        // スタックオーバーフロー→再起動していた)。
        characteristic.on_notify(move |raw| {
            let now = now_ms();
            if let Some(pair) = parse_measurement(kind, raw, now) {
                if let Ok(mut buf) = buffer.lock() {
                    buf.push(pair);
                }
            }
            last_rx.store(now as u32, Ordering::SeqCst);
            got_data.store(true, Ordering::SeqCst);
        });
    }

    // 体温計/血圧計の Measurement は indication ベースの機器が多い
    // (Arduino 版は canIndicate() 優先で登録)。CCCD はニプロ機では応答なしの書き込み、
    // Omron 機は応答あり (Write Request) でないと購読を無視する (Refs #237)
    let cccd_response = omron.is_some();
    if omron.is_some() {
        // Omron 機は 0x2A35 だけの購読では記録を送らない。Linux で記録が届いた回と同じく、
        // 全 service の notify / indicate を GATT の並び順に全部購読する
        // (0x2A35 には上の受信コールバックが付いている)
        let n = omron_subscribe_all(client, &disconnected).await?;
        alc_hub_common::evtlog::emit(&format!("EVT OMRON_SUB n={n}"));
    } else if characteristic.can_indicate() {
        characteristic
            .subscribe_indicate(cccd_response)
            .await
            .context("indication 購読失敗")?;
    } else if characteristic.can_notify() {
        characteristic
            .subscribe_notify(cccd_response)
            .await
            .context("notification 購読失敗")?;
    } else {
        let _ = client.disconnect();
        anyhow::bail!("notify/indicate 非対応のキャラクタリスティック");
    }

    println!(
        "{{\"type\":\"connected\",\"device\":\"{}\"}}",
        kind.json_name()
    );
    if let Ok(mut st) = status.lock() {
        st.push_event(now_ms(), &format!("{} 接続", kind.jp_name()));
        st.ble_connected = true;
        st.ble_device = kind.json_name().to_string();
    }

    // データ受信を待つ:
    // - 受信あり: 続報 (血圧計の過去分ダンプ) が静穏時間途切れたら切断して転送
    // - 機器の自発切断: 受信済み分を転送して終了
    // - 無データのままタイムアウト: 張り付き防止のためこちらから切断
    // Omron 機は受信しても S3R から切らず、機器が切断するまで (上限 OMRON_SESSION_TIMEOUT_MS) 待つ
    let wait_start = now_ms();
    loop {
        if omron.is_some() {
            let by = if disconnected.is_set() {
                "peer"
            } else if now_ms().saturating_sub(wait_start) > OMRON_SESSION_TIMEOUT_MS {
                let _ = client.disconnect();
                "timeout"
            } else {
                FreeRtos::delay_ms(100);
                continue;
            };
            let n = buffer.lock().map(|b| b.len()).unwrap_or(0);
            if by == "timeout" && n == 0 {
                alc_hub_common::evtlog::emit("EVT OMRON_RX timeout");
            }
            alc_hub_common::evtlog::emit(&format!("EVT OMRON_END by={by} n={n}"));
            println!(
                "{{\"type\":\"disconnected\",\"device\":\"{}\"}}",
                kind.json_name()
            );
            break;
        }
        if disconnected.is_set() {
            println!(
                "{{\"type\":\"disconnected\",\"device\":\"{}\"}}",
                kind.json_name()
            );
            break;
        }
        if got_data.load(Ordering::SeqCst) {
            let quiet =
                u64::from((now_ms() as u32).wrapping_sub(last_rx.load(Ordering::SeqCst)));
            if quiet >= data_quiet_ms(kind) {
                let _ = client.disconnect();
                break;
            }
        } else if now_ms().saturating_sub(wait_start) > DATA_WAIT_TIMEOUT_MS {
            let _ = client.disconnect();
            println!(
                "{{\"type\":\"disconnected\",\"device\":\"{}\",\"reason\":\"timeout\"}}",
                kind.json_name()
            );
            break;
        }
        FreeRtos::delay_ms(100);
    }

    // 貯めた測定のうち最新 (order 最大) の 1 件だけを recorder へ送る。
    // 血圧計の過去分ダンプから「今測った 1 件」を選ぶ。
    let latest = buffer
        .lock()
        .ok()
        .and_then(|buf| buf.iter().max_by_key(|(_, order)| *order).map(|(m, _)| *m));
    if let Some(m) = latest {
        if let Ok(tx) = meas_tx.lock() {
            let _ = tx.send(m);
        }
    }
    Ok(got_data.load(Ordering::SeqCst))
}

/// Omron 機のペアリング待ち (-P- 点滅) に接続した直後に呼ぶ。機種の系統で手順が違い、
/// 独自 service が 16bit `0xFE4A` の系統 (HCR-1901T2) は **bond だけ**、128bit の
/// [`OMRON_SERVICE`] の系統 (HEM-6231T) は RX[0] 購読 → bond → unlock 購読 →
/// プログラムモード → 鍵の登録。各段を `EVT OMRON_PAIR <段> ok|err` で出す。
/// 登録後の受信は標準 0x2A35 で鍵を使わないので、鍵は毎回乱数で作って保存しない
/// (値はログに出さない)。切断は呼び出し側。
///
/// 戻り値は**この接続をそのまま購読に使うか** — bls 系統は `true` (機器は bond だけで
/// 切られると登録が終わったと見なさず `-P-` が消えない)、legacy 系統は `false`
async fn omron_pair(client: &mut BLEClient, disconnected: &Disconnected) -> Result<bool> {
    fn step<T, E: core::fmt::Debug>(stage: &str, res: Result<T, E>) -> Result<T> {
        match res {
            Ok(v) => {
                alc_hub_common::evtlog::emit(&format!("EVT OMRON_PAIR {stage} ok"));
                Ok(v)
            }
            Err(e) => {
                alc_hub_common::evtlog::emit(&format!("EVT OMRON_PAIR {stage} err"));
                Err(anyhow::anyhow!("Omron ペアリング {stage} 失敗: {e:?}"))
            }
        }
    }

    // 系統の判定。16bit 0xFE4A が在れば HCR-1901T2 系 — unlock の電文には応答せず
    // (02+16×00 を 10 回書いても無応答。Windows で実測)、bond だけで標準 BLS を流す。
    // 見つからなければ従来どおり HEM-6231T の手順に進む (判定に失敗した回も同じ)
    let bls_only = matches!(
        disconnected
            .until(async {
                client
                    .get_service(BleUuid::from_uuid16(OMRON_SERVICE_16))
                    .await
                    .map(|_| ())
            })
            .await,
        Ok(Ok(()))
    );
    if bls_only {
        alc_hub_common::evtlog::emit("EVT OMRON_PAIR style=bls");
        // 既定の設定 (`set_auth(AuthReq::Bond)` は sm_sc=0、配布鍵は our=ENC のみ) では
        // NimBLE 側が bond 成立を返しても機器は -P- を消さない = ボンドと認めない。
        // Windows が成功した条件 (LE Secure Connections + IRK の配布/受け入れ) に寄せる
        BLEDevice::take()
            .security()
            .set_auth(AuthReq::Bond | AuthReq::Sc)
            .resolve_rpa();
        // bond だけ張る。RX[0] の購読は bond 前だと Insufficient Authentication で
        // 弾かれるので行わない (機器側からの Security Request も待たず、こちらから張る)
        let res = disconnected
            .until_timeout(client.secure_connection(), OMRON_SECURE_TIMEOUT_MS)
            .await;
        step("bond", res.and_then(|r| Ok(r?)))?;
        // 切らずに戻る。呼び出し側がこの接続のまま 0x2A35 / 0x2A2B を購読し、機器が
        // 記録を送って自分で切断したところで登録が完了する (esphome-omron が言う
        // 「bond して登録して読むまでを 1 セッションで」と同じ)
        return Ok(true);
    }
    alc_hub_common::evtlog::emit("EVT OMRON_PAIR style=legacy");
    // HEM-6231T は従来の条件 (legacy pairing / 配布鍵は既定) のまま張る。
    // bls 経路が広げた設定を持ち越さない
    BLEDevice::take().security().set_auth(AuthReq::Bond);

    // 各段は切断でも抜ける (Disconnected::until)。抜けた段も err として出す
    // 1. RX[0] を購読する (CCCD は Write Request)。これで機器が bond を求めてくる
    let res = disconnected
        .until(async {
            client
                .get_service(OMRON_SERVICE)
                .await?
                .get_characteristic(OMRON_RX0)
                .await?
                .subscribe_notify(true)
                .await
        })
        .await
        .and_then(|r| Ok(r?));
    step("rx0_sub", res)?;

    // 2. bond。機器は Legacy / Just Works で応じる。失敗しても続ける (次段の書き込みで分かる)。
    // ただし切断・時間切れ (OMRON_SECURE_TIMEOUT_MS) は打ち切る (切断は呼び出し側)
    let res = disconnected
        .until_timeout(client.secure_connection(), OMRON_SECURE_TIMEOUT_MS)
        .await;
    let aborted = res.is_err();
    if let Err(e) = step("bond", res.and_then(|r| Ok(r?))) {
        if aborted {
            return Err(e);
        }
        log::warn!("ble: {e:?}");
    }

    // 3. unlock を購読し、応答 notify の先頭 2 byte を受け取れるようにする
    // (bit16 = 受信あり。nimble_host タスク上で走るので値を置くだけ)
    let ack = Arc::new(AtomicU32::new(0));
    let res = disconnected
        .until(async {
            let unlock = client
                .get_service(OMRON_SERVICE)
                .await?
                .get_characteristic(OMRON_UNLOCK)
                .await?;
            let ack = Arc::clone(&ack);
            unlock.on_notify(move |raw| {
                if let [a, b, ..] = raw {
                    ack.store(
                        0x1_0000 | (u32::from(*a) << 8) | u32::from(*b),
                        Ordering::SeqCst,
                    );
                }
            });
            unlock.subscribe_notify(true).await
        })
        .await
        .and_then(|r| Ok(r?));
    step("unlock_sub", res)?;

    // 4. プログラムモード。応答が来なければ 1 秒おきに繰り返す
    let mut res = Err(anyhow::anyhow!("応答なし"));
    for _ in 0..OMRON_PROGRAM_MODE_TRIES {
        if let Err(e) =
            omron_unlock_write(client, disconnected, &ack, OMRON_OP_PROGRAM_MODE, &[0; 16]).await
        {
            res = Err(e);
            break;
        }
        if wait_omron_ack(&ack, disconnected, OMRON_PROGRAM_MODE_WAIT_MS)
            == Some(OMRON_ACK_PROGRAM_MODE)
        {
            res = Ok(());
            break;
        }
    }
    step("program_mode", res)?;

    // 5. 乱数の鍵を登録する。応答が来ると機器の -P- が消える
    let mut key = [0u8; 16];
    unsafe { esp_idf_svc::sys::esp_fill_random(key.as_mut_ptr().cast(), key.len()) };
    let res = omron_unlock_write(client, disconnected, &ack, OMRON_OP_SET_KEY, &key)
        .await
        .and_then(|()| {
            (wait_omron_ack(&ack, disconnected, OMRON_SET_KEY_WAIT_MS) == Some(OMRON_ACK_SET_KEY))
                .then_some(())
                .context("応答なし")
        });
    step("key", res)?;

    // 6. 後始末は Linux (omblepy と同じ手順) に合わせる: unlock → RX[0] の順に CCCD を 0 に
    // (Write Request) してから 3 秒待つ。切断は呼び出し側。登録は済んでいるので、ここで
    // 失敗しても Err にはしない
    let res = disconnected
        .until(async {
            client
                .get_service(OMRON_SERVICE)
                .await?
                .get_characteristic(OMRON_UNLOCK)
                .await?
                .unsubscribe(true)
                .await?;
            client
                .get_service(OMRON_SERVICE)
                .await?
                .get_characteristic(OMRON_RX0)
                .await?
                .unsubscribe(true)
                .await
        })
        .await
        .and_then(|r| Ok(r?));
    if let Err(e) = step("unsub", res) {
        log::warn!("ble: {e:?}");
    }
    let start = now_ms();
    while !disconnected.is_set() && now_ms().saturating_sub(start) < OMRON_PAIR_LINGER_MS {
        FreeRtos::delay_ms(100);
    }
    Ok(false)
}

/// その機器の bond だけを消し、`EVT OMRON_PAIR unbond ok|err|none` を出す
/// (全消去はニプロ機の bond を巻き込むので使わない)
fn omron_unbond(addr: &BLEAddress) {
    let result = match omron_find_bond(addr) {
        Ok(Some(bonded)) => match BLEDevice::take().delete_bond(&bonded) {
            Ok(()) => "ok",
            Err(e) => {
                log::warn!("ble: Omron bond 消去失敗: {e:?}");
                "err"
            }
        },
        Ok(None) => "none",
        Err(e) => {
            log::warn!("ble: {e:?}");
            "err"
        }
    };
    alc_hub_common::evtlog::emit(&format!("EVT OMRON_PAIR unbond {result}"));
}

/// 血圧計としてボンドした機器のアドレスを NVS へ記録する (同じ値なら書かない)。
/// **真偽はここに持たない** — 現在値は [`bp_bond_parts`] が毎回決める (Refs #249)。
/// アドレスは端末の識別子になりうるのでログには出さない
fn remember_bp_bond(settings: &Settings, addr: &BLEAddress, recorded: &mut Option<[u8; 6]>) {
    let val = addr.as_le_bytes();
    if *recorded == Some(val) {
        return;
    }
    match settings.set_bp_bond_addr(&val) {
        Ok(()) => {
            *recorded = Some(val);
            alc_hub_common::evtlog::emit("EVT BP_BOND saved");
        }
        Err(e) => log::warn!("ble: bp_bond 記録失敗: {e:?}"),
    }
}

/// 血圧計のボンド判定を内訳つきで返す — `(記録が在るか, 記録したアドレスが
/// ボンド一覧に居るか)`。2 つ目がそのまま「ボンドされているか」で、判定そのものは
/// hub-core の純粋関数が持つ。記録が無ければ NimBLE には問い合わせない
/// (スキャン 1 周ごとに呼ぶため) ので、そのときの 2 つ目は `false`
fn bp_bond_parts(recorded: Option<[u8; 6]>) -> (bool, bool) {
    let has_record = recorded.is_some();
    let bonded = has_record && alc_hub_core::device::bp_bonded(recorded, &bonded_addrs());
    (has_record, bonded)
}

/// NimBLE が今持っている bond のアドレス (native の little endian 6 B)。
/// 取得に失敗したら空 = 「ボンドされていない」側に倒す
fn bonded_addrs() -> Vec<[u8; 6]> {
    BLEDevice::take()
        .bonded_addresses()
        .map(|addrs| addrs.iter().map(BLEAddress::as_le_bytes).collect())
        .unwrap_or_default()
}

/// 保存済みの bond からその機器のアドレスを探す。BLEAddress の == は 6 byte だけを比べるので、
/// 返すのは保存側のアドレス (型付き。delete_bond にはこちらを渡す)
fn omron_find_bond(addr: &BLEAddress) -> Result<Option<BLEAddress>> {
    let addrs = BLEDevice::take()
        .bonded_addresses()
        .context("bond 一覧の取得失敗")?;
    Ok(addrs.into_iter().find(|a| a == addr))
}

/// `connect` を待ちながら、`encrypt` なら接続ができた瞬間に暗号化を始める (MTU 交換を待たない。
/// Linux で記録が届いた接続は、接続から約 50 ms で暗号化を始めていた)。esp32-nimble の
/// on_connect は MTU 交換の完了後に呼ばれるので使えない。connect が client を借りているので、
/// 接続の有無と conn_handle は NimBLE の conn desc をアドレスで引いて見る (5 ms おき)。
/// 暗号化を始めたら `EVT OMRON_ENC start rc=<n> retry=<k>` を出し、その結果を返す。
/// 暗号化の完了前に接続が消えたら (NimBLE が 0x3e で接続を張り直す。S3R で実測)、
/// 張り直された接続が見えた時点で暗号化を始め直す (retry を 1 増やす)
async fn connect_encrypting<F: Future>(
    connect: F,
    addr: BLEAddress,
    encrypt: bool,
) -> (F::Output, Option<EarlyEnc>) {
    let mut connect = pin!(connect);
    let peer: esp_idf_svc::sys::ble_addr_t = addr.into();
    let mut early: Option<EarlyEnc> = None;
    poll_fn(|cx| {
        if encrypt {
            let mut desc = esp_idf_svc::sys::ble_gap_conn_desc::default();
            let found =
                unsafe { esp_idf_svc::sys::ble_gap_conn_find_by_addr(&peer, &mut desc) } == 0;
            match &mut early {
                None if found => {
                    let rc =
                        unsafe { esp_idf_svc::sys::ble_gap_security_initiate(desc.conn_handle) };
                    alc_hub_common::evtlog::emit(&format!("EVT OMRON_ENC start rc={rc} retry=0"));
                    early = Some(EarlyEnc {
                        rc,
                        encrypted: false,
                        retry: 0,
                        lost: false,
                    });
                }
                // 暗号化の完了前に接続が消えた
                Some(e) if !found && !e.encrypted => {
                    e.lost = true;
                }
                // 張り直された接続が見えたので、暗号化を始め直す
                Some(e) if found && e.lost => {
                    let rc =
                        unsafe { esp_idf_svc::sys::ble_gap_security_initiate(desc.conn_handle) };
                    e.retry += 1;
                    alc_hub_common::evtlog::emit(&format!(
                        "EVT OMRON_ENC start rc={rc} retry={}",
                        e.retry
                    ));
                    e.rc = rc;
                    e.lost = false;
                }
                // 暗号化が connect (MTU 待ち) の間に終わった
                Some(e)
                    if found && e.rc == 0 && !e.encrypted && desc.sec_state.encrypted() != 0 =>
                {
                    e.encrypted = true;
                }
                _ => {}
            }
        }
        if let Poll::Ready(out) = connect.as_mut().poll(cx) {
            return Poll::Ready((out, early));
        }
        // 接続の検出・消失と暗号化の完了を 5 ms おきに見る
        if encrypt && early.map_or(true, |e| !e.encrypted) {
            FreeRtos::delay_ms(5);
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    })
    .await
}

/// 接続確立の瞬間に始めた暗号化の結果
#[derive(Clone, Copy)]
struct EarlyEnc {
    /// ble_gap_security_initiate の戻り値
    rc: i32,
    /// connect の間に暗号化の完了が見えた
    encrypted: bool,
    /// 暗号化を始め直した回数
    retry: u32,
    /// 暗号化の完了前に接続が消え、張り直しを待っている
    lost: bool,
}

/// リンクが暗号化されるまで最大 `timeout_ms` 待つ (切断されたら Err)。OMRON_ENC_RESTART_MS の間
/// 暗号化されなければ、暗号化の手順が接続の張り直しで失われた場合に備えて始め直す
/// (手順が進行中なら NimBLE は EALREADY を返すだけ)
fn wait_encrypted(
    client: &BLEClient,
    disconnected: &Disconnected,
    timeout_ms: u64,
    early: &mut EarlyEnc,
) -> Result<()> {
    let start = now_ms();
    let mut last_start = start;
    loop {
        match client.desc() {
            Ok(d) if d.encrypted() => return Ok(()),
            Ok(d) if now_ms().saturating_sub(last_start) >= OMRON_ENC_RESTART_MS => {
                let rc = unsafe { esp_idf_svc::sys::ble_gap_security_initiate(d.conn_handle()) };
                early.retry += 1;
                alc_hub_common::evtlog::emit(&format!(
                    "EVT OMRON_ENC start rc={rc} retry={}",
                    early.retry
                ));
                last_start = now_ms();
            }
            _ => {}
        }
        if disconnected.is_set() {
            anyhow::bail!("切断された");
        }
        if now_ms().saturating_sub(start) >= timeout_ms {
            anyhow::bail!("暗号化の時間切れ ({timeout_ms} ms)");
        }
        FreeRtos::delay_ms(5);
    }
}

/// early start のあとのサービス探索を、見つかった数が落ち着くまで待つ。client の signal は
/// MTU 完了と ENC_CHANGE の両方が送り、connect はその片方しか受け取らないので、残った値で
/// get_services の待ちが探索の途中で戻り得る (探索のコールバックはその後も一覧を埋める)
async fn omron_settle_services(client: &mut BLEClient, disconnected: &Disconnected) -> Result<()> {
    disconnected
        .until(async { client.get_services().await.map(|it| it.len()) })
        .await?
        .context("サービス探索失敗")?;
    let start = now_ms();
    let mut last = usize::MAX;
    let mut stable_since = start;
    loop {
        let n = client.get_services().await.map(|it| it.len()).unwrap_or(0);
        if n != last {
            last = n;
            stable_since = now_ms();
        } else if now_ms().saturating_sub(stable_since) >= OMRON_SERVICES_SETTLE_MS {
            return Ok(());
        }
        if disconnected.is_set() {
            anyhow::bail!("切断された");
        }
        if now_ms().saturating_sub(start) >= OMRON_SECURE_TIMEOUT_MS {
            return Ok(());
        }
        FreeRtos::delay_ms(100);
    }
}

/// unlock に `op` + 16 byte を Write Request で書く (前回の応答は捨てる)
async fn omron_unlock_write(
    client: &mut BLEClient,
    disconnected: &Disconnected,
    ack: &AtomicU32,
    op: u8,
    body: &[u8; 16],
) -> Result<()> {
    let mut frame = [0u8; 17];
    frame[0] = op;
    frame[1..].copy_from_slice(body);
    ack.store(0, Ordering::SeqCst);
    disconnected
        .until(async {
            client
                .get_service(OMRON_SERVICE)
                .await?
                .get_characteristic(OMRON_UNLOCK)
                .await?
                .write_value(&frame, true)
                .await
        })
        .await?
        .context("Omron unlock 書き込み失敗")
}

/// unlock の応答 notify を最大 `timeout_ms` 待ち、先頭 2 byte を返す (切断されたら None)
fn wait_omron_ack(
    ack: &AtomicU32,
    disconnected: &Disconnected,
    timeout_ms: u64,
) -> Option<[u8; 2]> {
    let start = now_ms();
    loop {
        let v = ack.load(Ordering::SeqCst);
        if v != 0 {
            return Some([(v >> 8) as u8, v as u8]);
        }
        if disconnected.is_set() || now_ms().saturating_sub(start) >= timeout_ms {
            return None;
        }
        FreeRtos::delay_ms(50);
    }
}

/// Omron 機の notify / indicate を全部 Write Request で購読する。購読できた数を返す
/// (失敗した 1 本は飛ばす)。順番は固定: 0x2A35 → 他の service (並び順) → 0x2A2B を最後。
/// Linux で記録が届いた接続は 0x2A35 の CCCD を 0x2A2B より先に書いていて、逆順 (ESP32 の
/// ハンドル順) の接続では 0x2A2B の notify すら来なかった。機器は 0x2A2B の購読を送信開始の
/// 合図にしていて、そのとき 0x2A35 が未購読だと何も送らないと読める (Refs #237)。
/// 1 本ごとに `EVT OMRON_SUB chr=<uuid> ok|err` を出す
async fn omron_subscribe_all(client: &mut BLEClient, disconnected: &Disconnected) -> Result<usize> {
    fn log_sub(uuid: BleUuid, res: &Result<(), esp32_nimble::BLEError>) -> usize {
        let ok = match res {
            Ok(()) => true,
            Err(e) => {
                log::warn!("ble: Omron {uuid} の購読失敗: {e:?}");
                false
            }
        };
        let result = if ok { "ok" } else { "err" };
        alc_hub_common::evtlog::emit(&format!("EVT OMRON_SUB chr={uuid} {result}"));
        usize::from(ok)
    }

    let bls_uuid = BleUuid::from_uuid16(BLOOD_PRESSURE_SERVICE);
    let bpm_uuid = BleUuid::from_uuid16(BLOOD_PRESSURE_MEASUREMENT);
    let cts_uuid = BleUuid::from_uuid16(CURRENT_TIME_SERVICE);
    let ct_uuid = BleUuid::from_uuid16(CURRENT_TIME);
    disconnected
        .until(async {
            let mut n = 0;

            // 1. 0x2A35 を最初に (on_notify は handle_device が付けた受信コールバックを残す)
            let res = async {
                client
                    .get_service(bls_uuid)
                    .await?
                    .get_characteristic(bpm_uuid)
                    .await?
                    .subscribe_indicate(true)
                    .await
            }
            .await;
            n += log_sub(bpm_uuid, &res);

            // 2. 0x1810 と 0x1805 以外を並び順に
            let services: Vec<_> = client.get_services().await?.collect();
            for svc in services {
                let svc_uuid = svc.uuid();
                if svc_uuid == bls_uuid || svc_uuid == cts_uuid {
                    continue;
                }
                let chars = match svc.get_characteristics().await {
                    Ok(chars) => chars,
                    Err(e) => {
                        log::warn!("ble: Omron {svc_uuid} の列挙失敗: {e:?}");
                        continue;
                    }
                };
                for chr in chars {
                    if !(chr.can_indicate() || chr.can_notify()) {
                        continue;
                    }
                    let chr_uuid = chr.uuid();
                    let res = if chr.can_indicate() {
                        chr.subscribe_indicate(true).await
                    } else {
                        chr.subscribe_notify(true).await
                    };
                    n += log_sub(chr_uuid, &res);
                }
            }

            // 3. 0x2A2B を最後に
            let res = async {
                client
                    .get_service(cts_uuid)
                    .await?
                    .get_characteristic(ct_uuid)
                    .await?
                    .subscribe_notify(true)
                    .await
            }
            .await;
            n += log_sub(ct_uuid, &res);

            anyhow::Ok(n)
        })
        .await?
}

/// on_disconnect で立つ印。立ったら待っている future を起こす。
/// esp32-nimble の await の中には切断で完了しないもの (connect の MTU 待ち) があるため、
/// Omron の経路と接続はこれで包み、切断されたら必ず戻る
#[derive(Default)]
struct Disconnected {
    flag: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Disconnected {
    /// on_disconnect (nimble_host タスク) から呼ぶ
    fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(w) = self.waker.lock().ok().and_then(|mut w| w.take()) {
            w.wake();
        }
    }

    fn clear(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// `fut` を待つ。先に切断されたら Err で戻る
    async fn until<F: Future>(&self, fut: F) -> Result<F::Output> {
        let mut fut = pin!(fut);
        poll_fn(|cx| {
            // 印を見る前に waker を置く (見た直後に立っても起こされる)
            if let Ok(mut w) = self.waker.lock() {
                *w = Some(cx.waker().clone());
            }
            if self.is_set() {
                return Poll::Ready(Err(anyhow::anyhow!("切断された")));
            }
            fut.as_mut().poll(cx).map(Ok)
        })
        .await
    }

    /// `fut` を最大 `timeout_ms` 待つ。先に切断されたか時間切れなら Err で戻る
    async fn until_timeout<F: Future>(&self, fut: F, timeout_ms: u64) -> Result<F::Output> {
        let mut timer = EspTaskTimerService::new()?.timer_async()?;
        let mut sleep = pin!(timer.after(Duration::from_millis(timeout_ms)));
        let mut guarded = pin!(self.until(fut));
        poll_fn(|cx| {
            if let Poll::Ready(res) = guarded.as_mut().poll(cx) {
                return Poll::Ready(res);
            }
            sleep
                .as_mut()
                .poll(cx)
                .map(|_| Err(anyhow::anyhow!("時間切れ ({timeout_ms} ms)")))
        })
        .await
    }
}

/// notify コールバック用の軽量パース: raw → (Measurement, 並び順キー)。
/// 並び順キーは「最新の 1 件」を選ぶための比較値。機器タイムスタンプが
/// あればそれ (過去分より今の測定が大きくなる)、無ければ受信時刻 (last-wins)。
/// タイムスタンプは recorder の重複排除にも渡す。重い処理は recorder 側で行う。
fn parse_measurement(kind: DeviceKind, raw: &[u8], at_ms: u64) -> Option<(Measurement, u64)> {
    match kind {
        DeviceKind::Thermometer => parse_temperature(raw).map(|t| {
            let order = t.timestamp.unwrap_or(at_ms);
            (
                Measurement::Temperature {
                    celsius: t.celsius,
                    timestamp: t.timestamp,
                    at_ms,
                },
                order,
            )
        }),
        DeviceKind::BloodPressure => parse_blood_pressure(raw).map(|bp| {
            let order = bp.timestamp.unwrap_or(at_ms);
            (
                Measurement::BloodPressure {
                    systolic: bp.systolic,
                    diastolic: bp.diastolic,
                    pulse: bp.pulse,
                    timestamp: bp.timestamp,
                    at_ms,
                },
                order,
            )
        }),
    }
}

// 値のデコード (IEEE 11073) は alc-hub-core::ieee11073 に分離
// (ホストでの単体テスト・coverage 100% 対象)
