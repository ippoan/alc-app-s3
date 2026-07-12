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
    vitals,
};
use anyhow::{Context, Result};
use esp32_nimble::{
    enums::{AuthReq, SecurityIOCap},
    utilities::BleUuid,
    BLEAdvertisedData, BLEAdvertisedDevice, BLEClient, BLEDevice, BLEScan,
};
use esp_idf_svc::hal::{delay::FreeRtos, task::block_on};

use alc_hub_common::status::{now_ms, SharedStatus};
use alc_hub_common::ui_api::UiCommand;

/// on_notify クロージャ (Send + Sync 要求) から使うため Mutex で包む
type SharedTx = Arc<Mutex<Sender<UiCommand>>>;

const HEALTH_THERMOMETER_SERVICE: u16 = 0x1809;
const BLOOD_PRESSURE_SERVICE: u16 = 0x1810;
const TEMPERATURE_MEASUREMENT: u16 = 0x2A1C;
const BLOOD_PRESSURE_MEASUREMENT: u16 = 0x2A35;

const SCAN_DURATION_MS: i32 = 3_000;
const SCAN_COOLDOWN_MS: u32 = 500;
const MIN_RSSI: i8 = -80;
const CONNECT_RETRIES: u32 = 3;
/// データ受信後、次の測定に備えて切断・再スキャンするまでの猶予 (Arduino 版準拠)
const RESET_DELAY_MS: u32 = 2_000;

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
    tx: Sender<UiCommand>,
    wifi_busy: Arc<AtomicBool>,
) -> Result<()> {
    let tx: SharedTx = Arc::new(Mutex::new(tx));
    std::thread::Builder::new()
        .name("ble".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            if let Err(e) = block_on(task(status, tx, wifi_busy)) {
                log::error!("ble: タスク異常終了: {e:?}");
                println!("{{\"type\":\"error\",\"message\":\"BLE task terminated\"}}");
            }
        })?;
    Ok(())
}

async fn task(status: SharedStatus, tx: SharedTx, wifi_busy: Arc<AtomicBool>) -> Result<()> {
    let device = BLEDevice::take();

    // Arduino 版と同等の Just Works ボンディング設定
    device
        .security()
        .set_auth(AuthReq::Bond)
        .set_io_cap(SecurityIOCap::NoInputNoOutput);

    let mut scan = BLEScan::new();
    loop {
        // Wi-Fi の接続/スキャン中は BLE スキャンを止め、コエグジストの
        // 電波取り合いで Wi-Fi 側が失敗しないようにする (wifi.rs 参照)
        while wifi_busy.load(Ordering::SeqCst) {
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
        if let Err(e) = handle_device(&mut client, &adv, kind, &status, &tx).await {
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
    tx: &SharedTx,
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

    let service = client
        .get_service(service_uuid(kind))
        .await
        .context("サービスが見つからない")?;
    let characteristic = service
        .get_characteristic(measurement_uuid(kind))
        .await
        .context("キャラクタリスティックが見つからない")?;

    let got_data = Arc::new(AtomicBool::new(false));
    {
        let got_data = Arc::clone(&got_data);
        let tx = Arc::clone(tx);
        let status = Arc::clone(status);
        characteristic.on_notify(move |raw| {
            emit_measurement(kind, raw, &tx, &status);
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

    // データ受信 (2 秒後に切断して再スキャン) か、機器側の切断まで待つ。
    // ニプロ機器は測定送信後に自分から切断することが多い。
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
        FreeRtos::delay_ms(100);
    }
    Ok(())
}

/// 測定値の処理: ホストへ JSON 出力 + イベントログ + 画面表示 (UiCommand)
fn emit_measurement(kind: DeviceKind, raw: &[u8], tx: &SharedTx, status: &SharedStatus) {
    match kind {
        DeviceKind::Thermometer => match parse_temperature(raw) {
            Some(t) => {
                println!("{{\"type\":\"temperature\",\"value\":{t:.1},\"unit\":\"celsius\"}}");
                if let Ok(mut st) = status.lock() {
                    st.push_event(now_ms(), &vitals::temp_event(t));
                }
                if let Ok(tx) = tx.lock() {
                    let _ = tx.send(UiCommand::Temperature { celsius: t });
                }
            }
            None => println!("{{\"type\":\"error\",\"message\":\"temperature parse failed\"}}"),
        },
        DeviceKind::BloodPressure => match parse_blood_pressure(raw) {
            Some(bp) => {
                match bp.pulse {
                    Some(p) if p > 0.0 => println!(
                        "{{\"type\":\"blood_pressure\",\"systolic\":{:.0},\"diastolic\":{:.0},\"pulse\":{:.0},\"unit\":\"mmHg\"}}",
                        bp.systolic, bp.diastolic, p
                    ),
                    _ => println!(
                        "{{\"type\":\"blood_pressure\",\"systolic\":{:.0},\"diastolic\":{:.0},\"unit\":\"mmHg\"}}",
                        bp.systolic, bp.diastolic
                    ),
                }
                if let Ok(mut st) = status.lock() {
                    st.push_event(now_ms(), &vitals::bp_event(bp.systolic, bp.diastolic, bp.pulse));
                }
                if let Ok(tx) = tx.lock() {
                    let _ = tx.send(UiCommand::BloodPressure {
                        systolic: bp.systolic,
                        diastolic: bp.diastolic,
                        pulse: bp.pulse,
                    });
                }
            }
            None => println!("{{\"type\":\"error\",\"message\":\"blood_pressure parse failed\"}}"),
        },
    }
}

// 値のデコード (IEEE 11073) は alc-hub-core::ieee11073 に分離
// (ホストでの単体テスト・coverage 100% 対象)
