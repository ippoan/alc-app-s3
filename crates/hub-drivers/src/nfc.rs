//! Unit NFC (ST25R3916, I2C) 読み取り検証 (issue #84 + plan/nfc-card-identity.md)。
//!
//! I2C バスの所有は C++ 側 (components/nfc_shim → M5UnitUnified) に持たせる。
//! ここでは esp-idf-hal の `I2cDriver` を作らず、I2C ポート番号 (I2C_NUM_1) と
//! GPIO 番号だけを FFI 越しに渡す — I2C0 (内部バス、電源IC/タッチ、main.rs) と
//! I2C1 (NFC 専用) を完全分離し、Rust/C++ 二重の I2C ドライバ install を避ける。
//!
//! 配線: DIN Base Port B (GPIO, G8/G9)。DIN Base Port C (G17/G18, UART) は
//! rs232.rs が FC-1200 通信に使用中のため使えない (issue #84 検討時に判明)。
//! G8/G9 のどちらが SDA/SCL かは Port B が汎用 GPIO 表記のため未確定 — bring-up
//! で ST25R3916 が ack しなければ `sda`/`scl` の実引数を入替えて再試行すること。
//!
//! 本検証パスは `SharedStatus::push_event` のみで結果を通知する (「ログ確認」
//! 画面に既存の rs232.rs 等と同じ形式で表示される)。Measurement 化・recorder
//! fan-out・WS uplink 連携は将来スコープ (plan doc「実装への含意」参照)。

use anyhow::{bail, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyIOPin, Pin};

use alc_hub_common::status::{now_ms, SharedStatus};

extern "C" {
    fn nfc_shim_init(i2c_port: i32, sda_gpio: i32, scl_gpio: i32) -> i32;
    fn nfc_shim_poll_felica_idm(out_hex: *mut u8, out_cap: i32) -> i32;
    fn nfc_shim_read_license_expiry(
        out_issue: *mut u8,
        issue_cap: i32,
        out_expiry: *mut u8,
        expiry_cap: i32,
    ) -> i32;
}

/// I2C_NUM_1 (ESP-IDF)。main.rs の内部バス (I2C_NUM_0, G12/G11) とは別ポート
const I2C_PORT_NFC: i32 = 1;
/// ポーリング間隔。rs232.rs の 100ms ブロッキング読出しより緩め (かざす操作は連続でない)
const POLL_INTERVAL_MS: u32 = 200;

pub fn start(sda: AnyIOPin, scl: AnyIOPin, status: SharedStatus) -> Result<()> {
    // Pin::pin() は PinId (u8) を返す。ownership は FFI 側 (C++/M5HAL) が握るため
    // 番号だけ取り出して drop する (esp-idf-hal 側では未使用)
    let sda_num = sda.pin() as i32;
    let scl_num = scl.pin() as i32;
    drop(sda);
    drop(scl);

    std::thread::Builder::new()
        .name("nfc".into())
        // APDU 組立 (String) + FFI 経由の hex 文字列バッファがあるため rs232.rs と同等
        .stack_size(8 * 1024)
        .spawn(move || run(sda_num, scl_num, status))?;
    Ok(())
}

fn run(sda_num: i32, scl_num: i32, status: SharedStatus) {
    let rc = unsafe { nfc_shim_init(I2C_PORT_NFC, sda_num, scl_num) };
    if rc != 0 {
        push_event(
            &status,
            &format!("NFC 初期化失敗 rc={rc} (配線/バス役割 sda={sda_num} scl={scl_num} を確認)"),
        );
        return;
    }
    push_event(&status, "NFC 待受開始 (Port B)");

    let mut last_idm: Option<String> = None;
    loop {
        match poll_felica_idm() {
            Ok(Some(idm)) => {
                if last_idm.as_deref() != Some(idm.as_str()) {
                    push_event(&status, &format!("NFC IDm={idm}"));
                    last_idm = Some(idm);
                }
            }
            Ok(None) => last_idm = None, // カードが離れたらデバウンス状態をリセット
            Err(e) => log::warn!("nfc: FeliCa poll error: {e:#}"),
        }
        FreeRtos::delay_ms(POLL_INTERVAL_MS);
    }
}

fn poll_felica_idm() -> Result<Option<String>> {
    let mut buf = [0u8; 32];
    let n = unsafe { nfc_shim_poll_felica_idm(buf.as_mut_ptr(), buf.len() as i32) };
    if n == 0 {
        return Ok(None);
    }
    if n < 0 {
        bail!("nfc_shim_poll_felica_idm rc={n}");
    }
    Ok(Some(
        String::from_utf8_lossy(&buf[..n as usize]).into_owned(),
    ))
}

/// 従来 IC 運転免許証の PIN なし有効期限読み取り (EF 2F01)。呼び出し元 (host コマンド等)
/// から明示的にトリガする想定 — 本検証パスの自動ポーリングには含めない
/// (NFC-F/NFC-B のモード切替を伴うため、常時ポーリングに混ぜると Suica 検出と競合する)
#[allow(dead_code)]
pub fn read_license_expiry(status: &SharedStatus) {
    let mut issue = [0u8; 16];
    let mut expiry = [0u8; 16];
    let rc = unsafe {
        nfc_shim_read_license_expiry(
            issue.as_mut_ptr(),
            issue.len() as i32,
            expiry.as_mut_ptr(),
            expiry.len() as i32,
        )
    };
    if rc != 0 {
        push_event(status, &format!("免許証 読み取り失敗 rc={rc}"));
        return;
    }
    let issue_s = cstr_bytes_to_str(&issue);
    let expiry_s = cstr_bytes_to_str(&expiry);
    push_event(status, &format!("免許 交付 {issue_s} 期限 {expiry_s}"));
}

fn cstr_bytes_to_str(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn push_event(status: &SharedStatus, line: &str) {
    if let Ok(mut st) = status.lock() {
        st.push_event(now_ms(), line);
    }
}
