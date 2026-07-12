//! 内蔵 BLE central: ニプロ体温計 NT-100B / 血圧計 NBP-1BLE の読み取り。
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
//! ```

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use alc_hub_core::{
    device::{match_device_name, DeviceKind},
    ieee11073::{parse_blood_pressure, parse_temperature},
};
use anyhow::{Context, Result};
use esp32_nimble::{
    enums::{AuthReq, SecurityIOCap},
    utilities::BleUuid,
    BLEAddress, BLEAdvertisedData, BLEAdvertisedDevice, BLEClient, BLEDevice, BLEScan,
};
use esp_idf_svc::hal::{delay::FreeRtos, task::block_on};

use alc_hub_common::control::PairFlag;
use alc_hub_common::measurement::Measurement;
use alc_hub_common::status::{now_ms, SharedStatus};
use alc_hub_common::ui_api::UiCommand;
use alc_hub_core::coex::RadioCoex;

/// on_notify クロージャ (Send + Sync 要求) から使うため Mutex で包む。
/// notify コールバックは nimble_host タスク上で走りスタックが小さいため、
/// ここでは「パースして Measurement を送るだけ」に留める (重い処理は recorder)。
type MeasTx = Arc<Mutex<Sender<Measurement>>>;

const HEALTH_THERMOMETER_SERVICE: u16 = 0x1809;
const BLOOD_PRESSURE_SERVICE: u16 = 0x1810;
const TEMPERATURE_MEASUREMENT: u16 = 0x2A1C;
const BLOOD_PRESSURE_MEASUREMENT: u16 = 0x2A35;

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
) -> Result<()> {
    let meas_tx: MeasTx = Arc::new(Mutex::new(meas_tx));
    std::thread::Builder::new()
        .name("ble".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            if let Err(e) = block_on(task(status, meas_tx, ui_tx, coex, pair_flag)) {
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
                    println!("EVT PAIR_CLEARED");
                }
                Err(e) => {
                    log::warn!("ble: ボンド消去失敗: {e:?}");
                    println!("EVT PAIR_ERR ボンド消去に失敗");
                }
            }
        }

        // Wi-Fi の接続/スキャン中 + Improv セッション中は BLE スキャンを
        // 止め、コエグジストの電波取り合いで Wi-Fi 側が失敗しないようにする
        while coex.ble_should_pause(now_ms()) {
            FreeRtos::delay_ms(200);
        }

        // バックオフ期限切れの機器を解放
        empty_backoff.retain(|(_, at)| now_ms().saturating_sub(*at) < EMPTY_BACKOFF_MS);

        // ニプロ機器は測定時にアドバタイズを開始するため、短いスキャンを
        // 繰り返して発見次第すぐ接続する (Arduino 版 loop() と同じ運用)。
        // 送信済み機器の広告にも接続する — 一度届いた測定は再送されず
        // 数秒の空接続で終わり、万一の再送は recorder の重複排除が破棄する
        let target = scan
            .active_scan(true)
            .interval(100)
            .window(99)
            .start(device, SCAN_DURATION_MS, |dev, data| {
                // 直近の接続がデータなしだった機器はバックオフ中 — 接続しない
                if empty_backoff.iter().any(|(a, _)| *a == dev.addr()) {
                    return None;
                }
                match_target(dev, &data).map(|kind| (*dev, kind))
            })
            .await
            .context("BLE スキャン失敗")?;

        let Some((adv, kind)) = target else {
            FreeRtos::delay_ms(SCAN_COOLDOWN_MS);
            continue;
        };

        println!("{{\"type\":\"found\",\"device\":\"{}\"}}", kind.json_name());
        // 接続開始を UI へ通知 → 点呼画面のラベル横に取得中スピナーを表示
        let _ = ui_tx.send(UiCommand::BleAcquiring { device: kind });

        let mut client = device.new_client();
        match handle_device(&mut client, &adv, kind, &status, &meas_tx).await {
            // データなし: しばらくこの機器への再接続を控える (機器を空ける)。
            // 接続失敗 (Err) はバックオフしない — 新規測定の広告での一時的な
            // 接続失敗もあり、その場合は即リトライで拾いたい
            Ok(false) => empty_backoff.push((adv.addr(), now_ms())),
            Ok(true) => {}
            Err(e) => {
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
/// (ニプロ機器が独自名を使う場合の対策)。
fn match_target(dev: &BLEAdvertisedDevice, data: &BLEAdvertisedData<&[u8]>) -> Option<DeviceKind> {
    if dev.rssi() < MIN_RSSI {
        return None;
    }

    if data.is_advertising_service(&BleUuid::from_uuid16(HEALTH_THERMOMETER_SERVICE)) {
        return Some(DeviceKind::Thermometer);
    }
    if data.is_advertising_service(&BleUuid::from_uuid16(BLOOD_PRESSURE_SERVICE)) {
        return Some(DeviceKind::BloodPressure);
    }

    if let Some(name) = data.name() {
        // name() は生バイト列 (&[u8]) を返す
        return match_device_name(&String::from_utf8_lossy(name));
    }
    None
}

/// 接続 → 購読 → 測定値の受信 → 切断。データを受信したかを返す
/// (false なら呼び出し側が短いバックオフを掛けて機器を空ける)
async fn handle_device(
    client: &mut BLEClient,
    adv: &BLEAdvertisedDevice,
    kind: DeviceKind,
    status: &SharedStatus,
    meas_tx: &MeasTx,
) -> Result<bool> {
    let disconnected = Arc::new(AtomicBool::new(false));
    {
        let disconnected = Arc::clone(&disconnected);
        client.on_disconnect(move |_| disconnected.store(true, Ordering::SeqCst));
    }
    client.on_connect(|client| {
        // Arduino 版と同様、接続直後に conn params を更新する
        if let Err(e) = client.update_conn_params(120, 120, 0, 60) {
            log::warn!("ble: update_conn_params 失敗: {e:?}");
        }
    });

    // Arduino 版と同様に最大 3 回リトライ
    let mut attempt = 0;
    loop {
        attempt += 1;
        match client.connect(&adv.addr()).await {
            Ok(()) => break,
            Err(e) if attempt < CONNECT_RETRIES => {
                log::warn!("ble: 接続リトライ {attempt}/{CONNECT_RETRIES}: {e:?}");
                FreeRtos::delay_ms(500);
            }
            Err(e) => return Err(e).context("接続失敗 (リトライ上限)"),
        }
    }

    // 明示的な secure_connection は行わない。血圧計 (NBP-1BLE) は
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
    // (Arduino 版は canIndicate() 優先で登録)
    if characteristic.can_indicate() {
        characteristic
            .subscribe_indicate(false)
            .await
            .context("indication 購読失敗")?;
    } else if characteristic.can_notify() {
        characteristic
            .subscribe_notify(false)
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
    let wait_start = now_ms();
    loop {
        if disconnected.load(Ordering::SeqCst) {
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
