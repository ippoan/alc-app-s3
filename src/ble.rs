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
use std::sync::Arc;

use anyhow::{Context, Result};
use esp32_nimble::{
    enums::{AuthReq, SecurityIOCap},
    utilities::BleUuid,
    BLEAdvertisedData, BLEAdvertisedDevice, BLEClient, BLEDevice, BLEScan,
};
use esp_idf_svc::hal::{delay::FreeRtos, task::block_on};

use crate::status::SharedStatus;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceKind {
    Thermometer,
    BloodPressure,
}

impl DeviceKind {
    fn json_name(self) -> &'static str {
        match self {
            Self::Thermometer => "thermometer",
            Self::BloodPressure => "blood_pressure",
        }
    }

    fn service(self) -> BleUuid {
        match self {
            Self::Thermometer => BleUuid::from_uuid16(HEALTH_THERMOMETER_SERVICE),
            Self::BloodPressure => BleUuid::from_uuid16(BLOOD_PRESSURE_SERVICE),
        }
    }

    fn characteristic(self) -> BleUuid {
        match self {
            Self::Thermometer => BleUuid::from_uuid16(TEMPERATURE_MEASUREMENT),
            Self::BloodPressure => BleUuid::from_uuid16(BLOOD_PRESSURE_MEASUREMENT),
        }
    }
}

pub fn start(status: SharedStatus) -> Result<()> {
    std::thread::Builder::new()
        .name("ble".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            if let Err(e) = block_on(task(status)) {
                log::error!("ble: タスク異常終了: {e:?}");
                println!("{{\"type\":\"error\",\"message\":\"BLE task terminated\"}}");
            }
        })?;
    Ok(())
}

async fn task(status: SharedStatus) -> Result<()> {
    let device = BLEDevice::take();

    // Arduino 版と同等の Just Works ボンディング設定
    device
        .security()
        .set_auth(AuthReq::Bond)
        .set_io_cap(SecurityIOCap::NoInputNoOutput);

    let mut scan = BLEScan::new();
    loop {
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
        if let Err(e) = handle_device(&mut client, &adv, kind, &status).await {
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
        if name.contains("NT-100") || name.contains("Thermo") {
            return Some(DeviceKind::Thermometer);
        }
        if name.contains("NBP-1") || name.contains("BP") || name.contains("Blood") {
            return Some(DeviceKind::BloodPressure);
        }
    }
    None
}

async fn handle_device(
    client: &mut BLEClient,
    adv: &BLEAdvertisedDevice,
    kind: DeviceKind,
    status: &SharedStatus,
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
        .get_service(kind.service())
        .await
        .context("サービスが見つからない")?;
    let characteristic = service
        .get_characteristic(kind.characteristic())
        .await
        .context("キャラクタリスティックが見つからない")?;

    let got_data = Arc::new(AtomicBool::new(false));
    {
        let got_data = Arc::clone(&got_data);
        characteristic.on_notify(move |raw| {
            emit_measurement(kind, raw);
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

fn emit_measurement(kind: DeviceKind, raw: &[u8]) {
    match kind {
        DeviceKind::Thermometer => match parse_temperature(raw) {
            Some(t) => println!("{{\"type\":\"temperature\",\"value\":{t:.1},\"unit\":\"celsius\"}}"),
            None => println!("{{\"type\":\"error\",\"message\":\"temperature parse failed\"}}"),
        },
        DeviceKind::BloodPressure => match parse_blood_pressure(raw) {
            Some(bp) => match bp.pulse {
                Some(p) if p > 0.0 => println!(
                    "{{\"type\":\"blood_pressure\",\"systolic\":{:.0},\"diastolic\":{:.0},\"pulse\":{:.0},\"unit\":\"mmHg\"}}",
                    bp.systolic, bp.diastolic, p
                ),
                _ => println!(
                    "{{\"type\":\"blood_pressure\",\"systolic\":{:.0},\"diastolic\":{:.0},\"unit\":\"mmHg\"}}",
                    bp.systolic, bp.diastolic
                ),
            },
            None => println!("{{\"type\":\"error\",\"message\":\"blood_pressure parse failed\"}}"),
        },
    }
}

// ---------------------------------------------------------------------------
// 値のデコード (Arduino 版 parseTemperature / parseBloodPressure の移植)
// ---------------------------------------------------------------------------

/// Temperature Measurement (0x2A1C): IEEE 11073 FLOAT (32bit)
fn parse_temperature(data: &[u8]) -> Option<f32> {
    if data.len() < 5 {
        return None;
    }
    let flags = data[0];
    let fahrenheit = flags & 0x01 != 0;

    let mut mantissa =
        i32::from(data[1]) | (i32::from(data[2]) << 8) | (i32::from(data[3]) << 16);
    if mantissa & 0x0080_0000 != 0 {
        mantissa |= 0xFF00_0000u32 as i32; // 符号拡張
    }
    let exponent = data[4] as i8;

    let mut t = mantissa as f32 * 10f32.powi(i32::from(exponent));
    if fahrenheit {
        t = (t - 32.0) * 5.0 / 9.0;
    }
    Some(t)
}

struct BloodPressure {
    systolic: f32,
    diastolic: f32,
    pulse: Option<f32>,
}

/// IEEE 11073 SFLOAT (16bit)
fn sfloat(lo: u8, hi: u8) -> f32 {
    let mut mantissa = i16::from(lo) | (i16::from(hi & 0x0F) << 8);
    if mantissa & 0x0800 != 0 {
        mantissa |= 0xF000u16 as i16; // 符号拡張
    }
    let mut exponent = (hi >> 4) as i8;
    if exponent & 0x08 != 0 {
        exponent |= 0xF0u8 as i8; // 符号拡張
    }
    f32::from(mantissa) * 10f32.powi(i32::from(exponent))
}

/// Blood Pressure Measurement (0x2A35)
fn parse_blood_pressure(data: &[u8]) -> Option<BloodPressure> {
    if data.len() < 7 {
        return None;
    }
    let flags = data[0];
    let is_kpa = flags & 0x01 != 0;
    let has_timestamp = flags & 0x02 != 0;
    let has_pulse = flags & 0x04 != 0;

    let mut systolic = sfloat(data[1], data[2]);
    let mut diastolic = sfloat(data[3], data[4]);
    // data[5..7] は Mean Arterial Pressure (未使用)

    let mut offset = 7;
    if has_timestamp {
        offset += 7; // タイムスタンプは 7 バイト
    }
    let pulse = if has_pulse && offset + 2 <= data.len() {
        Some(sfloat(data[offset], data[offset + 1]))
    } else {
        None
    };

    if is_kpa {
        systolic *= 7.50062;
        diastolic *= 7.50062;
    }
    Some(BloodPressure {
        systolic,
        diastolic,
        pulse,
    })
}
