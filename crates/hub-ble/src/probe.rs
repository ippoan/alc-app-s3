//! 測定モード (`probe` feature、issue #237): 見えた BLE 広告を serial に出す。
//!
//! 実機テストは Atom VoiceS3R (`src/bin/probe.rs`)、最終的な載せ先は CoreS3 なので、
//! scan / connect / bond / Omron のペアリングは lib.rs の本番経路をそのまま通し、
//! ここは広告を 1 行ずつ出すだけに留める。
//!
//! 出力は `PROBE ` で始まる 1 行 (grep で拾う):
//!
//! ```text
//! PROBE ADV addr=.. type=.. rssi=.. name=.. svc=[..] mfg=<hex>
//! ```

use std::fmt::Write as _;
use std::sync::Mutex;

use alc_hub_common::status::now_ms;
use esp32_nimble::{enums::AdvType, BLEAddress, BLEAdvertisedData, BLEAdvertisedDevice};

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

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
