//! 測定モード (`probe` feature、issue #237): 未対応の BLE 機器 (Omron HEM-6231T) の
//! 広告・GATT・bond の様子を serial に出す。
//!
//! 実機テストは Atom VoiceS3R (`src/bin/probe.rs`)、最終的な載せ先は CoreS3 なので、
//! scan / connect / bond は lib.rs の本番経路をそのまま通し、ここは「見えたものを出す」
//! だけに留める。**書き込みは一切しない** (unlock 鍵を含む)。
//!
//! 順番は「先に bond してから購読する」— 購読だけでは暗号化されず、機器が
//! 暗号化前に書いた CCCD を受け付けていない疑いがあるため (#c237-1 の実測)。
//!
//! 出力は全部 `PROBE ` で始まる 1 行 (grep で拾う):
//!
//! ```text
//! PROBE BOND stage=pre_subscribe result=Ok|Err(..)
//! PROBE BOND stage=pre_subscribe bonded=.. encrypted=.. authenticated=..
//! PROBE ADV addr=.. type=.. rssi=.. name=.. svc=[..] mfg=<hex>
//! PROBE GATT svc=.. chr=.. props=read|write|write_no_rsp|notify|indicate
//! PROBE SUB chr=.. kind=notify|indicate result=Ok|Err(..)
//! PROBE BOND stage=after_subscribe bonded=.. encrypted=.. authenticated=..
//! PROBE RX chr=.. hex=..
//! PROBE BP parsed=Some(..)|None
//! PROBE DISCONNECT stage=.. reason=..
//! PROBE DONE bls=true|false
//! ```

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use alc_hub_common::status::now_ms;
use anyhow::Result;
use esp32_nimble::{
    enums::AdvType, utilities::BleUuid, BLEAddress, BLEAdvertisedData, BLEAdvertisedDevice,
    BLEClient,
};
use esp_idf_svc::hal::delay::FreeRtos;

/// HEM-6231T を含む Omron の BLE 機器が広告で使う名前の接頭辞
const NAME_PREFIX: &str = "BLEsmart_";
/// 購読してから切断するまでの受信時間
const RX_TOTAL_MS: u64 = 60_000;

/// 1 回の scan 窓の中で出したアドレス。窓 (SCAN_DURATION_MS) を過ぎたら忘れる
static ADV_SEEN: Mutex<(u64, Vec<(BLEAddress, bool)>)> = Mutex::new((0, Vec::new()));

/// 見えた広告を、1 回の scan 窓の中でアドレスごとに 1 回だけ出す。
/// active scan では広告と scan response が別々に届き、名前は scan response 側に
/// 載ることがあるので、この 2 つは別に数える
pub fn log_adv(dev: &BLEAdvertisedDevice, data: &BLEAdvertisedData<&[u8]>) {
    let addr = dev.addr();
    let scan_rsp = dev.adv_type() == AdvType::ScanResponse;
    {
        let Ok(mut seen) = ADV_SEEN.lock() else {
            return;
        };
        let now = now_ms();
        if now.saturating_sub(seen.0) >= crate::SCAN_DURATION_MS as u64 {
            *seen = (now, Vec::new());
        }
        if seen.1.contains(&(addr, scan_rsp)) {
            return;
        }
        seen.1.push((addr, scan_rsp));
    }

    let name = data
        .name()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_default();
    let svc = data
        .service_uuids()
        .map(|u| u.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mfg = data
        .manufacture_data()
        .map(|m| {
            let mut s = format!("{:04x}", m.company_identifier);
            s.push_str(&hex(m.payload));
            s
        })
        .unwrap_or_default();
    println!(
        "PROBE ADV addr={addr} type={:?} rssi={} name={name} svc=[{svc}] mfg={mfg}",
        dev.adv_type(),
        dev.rssi()
    );
}

/// 広告名が Omron の BLE 機器なら true
pub fn match_name(name: &str) -> bool {
    name.starts_with(NAME_PREFIX)
}

/// 切断がどの段で起きたかの印 (on_disconnect は nimble_host タスクで走る)
#[derive(Clone, Copy)]
#[repr(u8)]
enum Stage {
    Bond = 0,
    Discover,
    Subscribe,
    Receive,
    Done,
}

impl Stage {
    fn name(v: u8) -> &'static str {
        match v {
            0 => "bond",
            1 => "discover",
            2 => "subscribe",
            3 => "receive",
            _ => "done",
        }
    }
}

/// connect 成功直後に呼ぶ: 先に bond → GATT 列挙 → notify/indicate を全部購読 →
/// 受信して切断。受信があったかを返す (handle_device と同じ意味)
pub async fn inspect(client: &mut BLEClient) -> Result<bool> {
    let stage = Arc::new(AtomicU8::new(Stage::Bond as u8));
    // handle_device の on_disconnect を置き換える (probe では inspect から戻らない)
    {
        let stage = Arc::clone(&stage);
        client.on_disconnect(move |reason| {
            let v = stage.swap(Stage::Done as u8 | 0x80, Ordering::SeqCst);
            println!(
                "PROBE DISCONNECT stage={} reason={reason}",
                Stage::name(v & 0x7f)
            );
        });
    }
    let disconnected = || stage.load(Ordering::SeqCst) & 0x80 != 0;
    let set_stage = |s: Stage| {
        let _ = stage.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
            (v & 0x80 == 0).then_some(s as u8)
        });
    };

    // 1. GATT 列挙より前に、先に bond する (購読だけでは暗号化されない機器がいる)
    let res = client.secure_connection().await;
    println!("PROBE BOND stage=pre_subscribe result={res:?}");
    log_desc("pre_subscribe", client);

    let got_rx = Arc::new(AtomicU8::new(0));
    let mut bls = false;
    let bls_uuid = BleUuid::from_uuid16(crate::BLOOD_PRESSURE_SERVICE);
    let bpm_uuid = BleUuid::from_uuid16(crate::BLOOD_PRESSURE_MEASUREMENT);

    // 2-3. 列挙しながら notify/indicate を全部購読する (ディスカバリはキャッシュされるので 1 回)
    set_stage(Stage::Discover);
    match client.get_services().await {
        Ok(services) => {
            let services: Vec<_> = services.collect();
            set_stage(Stage::Subscribe);
            for svc in services {
                let svc_uuid = svc.uuid();
                bls |= svc_uuid == bls_uuid;
                let chars = match svc.get_characteristics().await {
                    Ok(chars) => chars,
                    Err(e) => {
                        println!("PROBE GATT svc={svc_uuid} chr=- props=Err({e:?})");
                        continue;
                    }
                };
                for chr in chars {
                    let chr_uuid = chr.uuid();
                    let mut props = Vec::new();
                    for (on, label) in [
                        (chr.can_read(), "read"),
                        (chr.can_write(), "write"),
                        (chr.can_write_no_response(), "write_no_rsp"),
                        (chr.can_notify(), "notify"),
                        (chr.can_indicate(), "indicate"),
                    ] {
                        if on {
                            props.push(label);
                        }
                    }
                    println!(
                        "PROBE GATT svc={svc_uuid} chr={chr_uuid} props={}",
                        props.join("|")
                    );

                    if !(chr.can_notify() || chr.can_indicate()) || disconnected() {
                        continue;
                    }
                    let is_bpm = chr_uuid == bpm_uuid;
                    {
                        let got_rx = Arc::clone(&got_rx);
                        // nimble_host タスク上で走る — 16 進にして 1 行出すだけ
                        chr.on_notify(move |raw| {
                            got_rx.store(1, Ordering::SeqCst);
                            println!("PROBE RX chr={chr_uuid} hex={}", hex(raw));
                            if is_bpm {
                                let parsed = alc_hub_core::ieee11073::parse_blood_pressure(raw);
                                println!("PROBE BP parsed={parsed:?}");
                            }
                        });
                    }
                    let (kind, res) = if chr.can_indicate() {
                        ("indicate", chr.subscribe_indicate(false).await)
                    } else {
                        ("notify", chr.subscribe_notify(false).await)
                    };
                    println!("PROBE SUB chr={chr_uuid} kind={kind} result={res:?}");
                }
            }
        }
        Err(e) => println!("PROBE GATT svc=- chr=- props=Err({e:?})"),
    }

    // 4. 購読がすべて終わった時点の bond の状態を 1 回だけ出す (待たない)
    log_desc("after_subscribe", client);

    // 5. ここから RX_TOTAL_MS 受信して切断する
    set_stage(Stage::Receive);
    wait_until(now_ms() + RX_TOTAL_MS, &disconnected);
    if !disconnected() {
        set_stage(Stage::Done);
        let _ = client.disconnect();
    }
    println!("PROBE DONE bls={bls}");
    Ok(got_rx.load(Ordering::SeqCst) != 0)
}

/// `PROBE BOND stage=..` を出し、暗号化されているかを返す
fn log_desc(stage: &str, client: &BLEClient) -> bool {
    match client.desc() {
        Ok(d) => {
            println!(
                "PROBE BOND stage={stage} bonded={} encrypted={} authenticated={}",
                d.bonded(),
                d.encrypted(),
                d.authenticated()
            );
            d.encrypted()
        }
        Err(e) => {
            println!("PROBE BOND stage={stage} desc=Err({e:?})");
            false
        }
    }
}

/// 期限 (now_ms 基準) まで、または切断されるまで待つ
fn wait_until(deadline_ms: u64, disconnected: &impl Fn() -> bool) {
    while now_ms() < deadline_ms && !disconnected() {
        FreeRtos::delay_ms(100);
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
