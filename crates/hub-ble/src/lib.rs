//! 内蔵 BLE central: ニプロ体温計 NT-100B / 血圧計 NBP-1BLE の読み取り。
//!
//! `ippoan/ble-medical-gateway` からの移植:
//! - スキャン → 接続 → notify/indicate 購読の骨組み:
//!   `firmware-rust/src/main.rs` (esp32-nimble PoC, PR #2-#5)
//! - 値のデコード (IEEE 11073 FLOAT/SFLOAT)・JSON 出力・データ受信後の
//!   自動リセット運用: `src/main.cpp` (Arduino/NimBLE 版, ATOM Lite 実機実績)
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

use std::sync::atomic::{AtomicBool, Ordering};
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
    BLEAdvertisedData, BLEAdvertisedDevice, BLEClient, BLEDevice, BLEScan,
};
use esp_idf_svc::hal::{delay::FreeRtos, task::block_on};

use alc_hub_common::control::PairFlag;
use alc_hub_common::measurement::Measurement;
use alc_hub_common::status::{now_ms, SharedStatus};
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
/// データ受信後、次の測定に備えて切断・再スキャンするまでの猶予 (Arduino 版準拠)
const RESET_DELAY_MS: u32 = 2_000;
/// 接続後この時間データが来なければ諦めて切断・再スキャンする。
/// 無い場合、無言の機器に繋がると supervision timeout (~99秒) まで BLE ループ
/// 全体がブロックされ、体温も血圧も取れなくなる (実機ログで確認)。
const DATA_WAIT_TIMEOUT_MS: u64 = 5_000;

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
    coex: Arc<RadioCoex>,
    pair_flag: PairFlag,
) -> Result<()> {
    let meas_tx: MeasTx = Arc::new(Mutex::new(meas_tx));
    std::thread::Builder::new()
        .name("ble".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            if let Err(e) = block_on(task(status, meas_tx, coex, pair_flag)) {
                log::error!("ble: タスク異常終了: {e:?}");
                println!("{{\"type\":\"error\",\"message\":\"BLE task terminated\"}}");
            }
        })?;
    Ok(())
}

async fn task(
    status: SharedStatus,
    meas_tx: MeasTx,
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

        // ニプロ機器は測定時にアドバタイズを開始するため、短いスキャンを
        // 繰り返して発見次第すぐ接続する (Arduino 版 loop() と同じ運用)
        let target = scan
            .active_scan(true)
            .interval(100)
            .window(99)
            .start(device, SCAN_DURATION_MS, |dev, data| {
                match_target(dev, data).map(|kind| (*dev, kind))
            })
            .await
            .context("BLE スキャン失敗")?;

        let Some((adv, kind)) = target else {
            FreeRtos::delay_ms(SCAN_COOLDOWN_MS);
            continue;
        };

        println!("{{\"type\":\"found\",\"device\":\"{}\"}}", kind.json_name());

        let mut client = device.new_client();
        if let Err(e) = handle_device(&mut client, &adv, kind, &status, &meas_tx).await {
            log::warn!("ble: {} 処理失敗: {e:?}", kind.json_name());
            println!(
                "{{\"type\":\"error\",\"message\":\"{}: connection failed\"}}",
                kind.json_name()
            );
        }
        drop(client);

        if let Ok(mut st) = status.lock() {
            st.ble_connected = false;
            st.ble_device.clear();
        }
        println!("{{\"type\":\"reset\",\"message\":\"Scan restarted\"}}");
    }
}

/// 広告が対象サービス (体温計/血圧計) を含み RSSI が閾値以上なら種別を返す。
/// Arduino 版と同様、標準サービス UUID に加えてデバイス名でも判定する
/// (ニプロ機器が独自名を使う場合の対策)。
fn match_target(dev: &BLEAdvertisedDevice, data: BLEAdvertisedData<&[u8]>) -> Option<DeviceKind> {
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

async fn handle_device(
    client: &mut BLEClient,
    adv: &BLEAdvertisedDevice,
    kind: DeviceKind,
    status: &SharedStatus,
    meas_tx: &MeasTx,
) -> Result<()> {
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
    let buffer: Arc<Mutex<Vec<(Measurement, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let got_data = Arc::clone(&got_data);
        let buffer = Arc::clone(&buffer);
        // このクロージャは nimble_host タスク上で呼ばれる (スタック小)。
        // パースしてバッファに積むだけに留める — println!/format!/NVS 等の
        // 重い処理は recorder スレッドで行う (以前ここで直接やって血圧受信時に
        // スタックオーバーフロー→再起動していた)。
        characteristic.on_notify(move |raw| {
            if let Some(pair) = parse_measurement(kind, raw, now_ms()) {
                if let Ok(mut buf) = buffer.lock() {
                    buf.push(pair);
                }
            }
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

    // データ受信 (最初の受信から 2 秒 = 過去分の送信を受け切る猶予) か、
    // 機器側の切断、または無データのままタイムアウトするまで待つ。
    // ニプロ機器は測定送信後に自分から切断することが多い。
    let wait_start = now_ms();
    loop {
        if got_data.load(Ordering::SeqCst) {
            FreeRtos::delay_ms(RESET_DELAY_MS);
            let _ = client.disconnect();
            break;
        }
        if disconnected.load(Ordering::SeqCst) {
            println!(
                "{{\"type\":\"disconnected\",\"device\":\"{}\"}}",
                kind.json_name()
            );
            break;
        }
        // 接続したがデータが来ない機器に張り付いて BLE ループを固めないよう、
        // タイムアウトしたら切断して再スキャンへ戻る (体温計/血圧計の両方を救う)
        if now_ms().saturating_sub(wait_start) > DATA_WAIT_TIMEOUT_MS {
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
    Ok(())
}

/// notify コールバック用の軽量パース: raw → (Measurement, 並び順キー)。
/// 並び順キーは「最新の 1 件」を選ぶための比較値。血圧は機器タイムスタンプが
/// あればそれ (過去分より今の測定が大きくなる)、無ければ受信時刻 (last-wins)。
/// 体温は 1 件のみなので受信時刻。重い処理は recorder 側で行う。
fn parse_measurement(kind: DeviceKind, raw: &[u8], at_ms: u64) -> Option<(Measurement, u64)> {
    match kind {
        DeviceKind::Thermometer => parse_temperature(raw)
            .map(|celsius| (Measurement::Temperature { celsius, at_ms }, at_ms)),
        DeviceKind::BloodPressure => parse_blood_pressure(raw).map(|bp| {
            let order = bp.timestamp.unwrap_or(at_ms);
            (
                Measurement::BloodPressure {
                    systolic: bp.systolic,
                    diastolic: bp.diastolic,
                    pulse: bp.pulse,
                    at_ms,
                },
                order,
            )
        }),
    }
}

// 値のデコード (IEEE 11073) は alc-hub-core::ieee11073 に分離
// (ホストでの単体テスト・coverage 100% 対象)
