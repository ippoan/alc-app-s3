//! Unit NFC (ST25R3916, I2C) 読み取り (issue #84 / #96 / #101 + plan/nfc-card-identity.md)。
//!
//! I2C バスの所有は C++ 側 (components/nfc_shim → M5UnitUnified) に持たせる。
//! ここでは esp-idf-hal の `I2cDriver` を作らず、I2C ポート番号 (I2C_NUM_1) と
//! GPIO 番号だけを FFI 越しに渡す — I2C0 (内部バス、電源IC/タッチ、main.rs) と
//! I2C1 (NFC 専用) を完全分離し、Rust/C++ 二重の I2C ドライバ install を避ける。
//!
//! 配線: DIN Base Port A (SDA=G2 / SCL=G1、AtomS3 ベンチ (crates/atoms3-nfc) と
//! 同一ピン番号)。issue #101 の LAN Module 13.2 取り外し構成が前提 — Port B
//! (旧配線 G8/G9) は issue #84 検討時の暫定割当だった。ack しなければ
//! `sda`/`scl` の実引数を入替えて再試行すること。
//!
//! 存在検知ゲート + F(交通系IDm)→A(HCE/UID)→B(免許証) 逐次掃引は
//! crates/atoms3-nfc/src/main.rs (issue #96 で実機確認済み) の移植。
//! 通知は `SharedStatus::push_event` のみ (「ログ確認」画面に既存の rs232.rs
//! 等と同じ形式で表示される)。Measurement 化・recorder fan-out・WS uplink
//! 連携・スピーカービープ (issue #101 PR2) は将来スコープ。

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyIOPin, Pin};

use alc_hub_common::status::{now_ms, SharedStatus};

use crate::speaker::Speaker;

extern "C" {
    fn nfc_shim_init(i2c_port: i32, sda_gpio: i32, scl_gpio: i32) -> i32;
    fn nfc_shim_poll_felica_idm(out_hex: *mut u8, out_cap: i32) -> i32;
    fn nfc_shim_poll_nfca_uid(out_hex: *mut u8, out_cap: i32) -> i32;
    fn nfc_shim_read_license_expiry(
        out_issue: *mut u8,
        issue_cap: i32,
        out_expiry: *mut u8,
        expiry_cap: i32,
    ) -> i32;
    fn nfc_shim_measure_amplitude() -> i32;
    fn nfc_shim_measure_phase() -> i32;
}

/// I2C_NUM_1 (ESP-IDF)。main.rs の内部バス (I2C_NUM_0, G12/G11) とは別ポート
const I2C_PORT_NFC: i32 = 1;

/// 存在検知 (アンテナ振幅) のトリガ閾値。カード無しのベースラインは完全に
/// 安定 (AtomS3 実測: 60サンプル連続でノイズ0)、カード接近で 2 下がる。
/// |amp - baseline| がこの値以上で「何かかざされた」と判定し F→A→B の
/// 逐次ポーリングを開始する (issue #96 続き)。CoreS3 環境固有のベースライン
/// ノイズは heartbeat ログ (tick%100) で実機再確認が必要 (issue #101)
const PRESENCE_DELTA: i32 = 2;

// タップ運用 (かざしてすぐ離す) のため空白時間を最小化 (AtomS3 ベンチと同値)
const POLL_INTERVAL_MS: u32 = 20;

/// トリガ固着の保険: 何も読めないまま3秒続いたら誤トリガとみなし再較正
/// (温度ドリフト等でベースラインが実態とずれたケースの自己回復)
const TRIGGER_STUCK: Duration = Duration::from_secs(3);

pub fn start(
    sda: AnyIOPin,
    scl: AnyIOPin,
    status: SharedStatus,
    speaker: Speaker,
) -> Result<()> {
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
        .spawn(move || run(sda_num, scl_num, status, speaker))?;
    Ok(())
}

fn run(sda_num: i32, scl_num: i32, status: SharedStatus, mut speaker: Speaker) {
    let rc = unsafe { nfc_shim_init(I2C_PORT_NFC, sda_num, scl_num) };
    if rc != 0 {
        push_event(
            &status,
            &format!("NFC 初期化失敗 rc={rc} (配線/バス役割 sda={sda_num} scl={scl_num} を確認)"),
        );
        return;
    }
    push_event(&status, "NFC 待受開始 (存在検知ゲート + F→A→B 逐次ポーリング)");

    let mut last_idm: Option<String> = None;
    let mut last_uid: Option<String> = None;
    // -2 (カード無し) は定常状態なのでログしない。未実行センチネルは i32::MIN
    let mut last_license_rc = i32::MIN;

    // 存在検知のベースライン (-1 = 未較正、初回測定値で初期化)。
    // 振幅はカード系、位相はスマホ系 (モバイルSuica 等、振幅に出にくい) を拾う
    let mut baseline: i32 = -1;
    let mut baseline_ph: i32 = -1;
    let mut triggered_since: Option<Instant> = None;
    let mut tick: u32 = 0;

    loop {
        tick = tick.wrapping_add(1);

        // --- 待機: プロトコル非依存の存在検知 (アンテナ振幅+位相) ---
        // モード切替もポーリングも行わず振幅・位相だけを見る。ベースラインは
        // 非トリガ時のみ ±1 ずつ追従させ温度ドリフトを吸収する (カードが
        // 載っている間は追従しないので、置きっぱなしでも基準が汚れない)
        let amp = unsafe { nfc_shim_measure_amplitude() };
        let ph = unsafe { nfc_shim_measure_phase() };
        let mut triggered = false;
        if amp >= 0 {
            if baseline < 0 {
                baseline = amp;
            }
            if (amp - baseline).abs() >= PRESENCE_DELTA {
                triggered = true;
            } else {
                baseline += (amp - baseline).signum();
            }
        } else {
            triggered = true; // 測定失敗時は常時ポーリングへフォールバック (安全側)
        }
        if ph >= 0 {
            if baseline_ph < 0 {
                baseline_ph = ph;
            }
            if (ph - baseline_ph).abs() >= PRESENCE_DELTA {
                triggered = true;
            } else {
                baseline_ph += (ph - baseline_ph).signum();
            }
        }

        if tick % 100 == 0 {
            log::info!(
                "nfc heartbeat tick={tick} amp={amp}/{baseline} ph={ph}/{baseline_ph} last_rc={last_license_rc}"
            );
        }

        if !triggered {
            triggered_since = None;
            FreeRtos::delay_ms(POLL_INTERVAL_MS);
            continue;
        }
        match triggered_since {
            None => triggered_since = Some(Instant::now()),
            Some(t0) if t0.elapsed() > TRIGGER_STUCK => {
                log::info!("nfc presence: 再較正 amp={amp} ph={ph} (トリガ固着 {TRIGGER_STUCK:?})");
                baseline = amp;
                baseline_ph = ph;
                triggered_since = None;
                FreeRtos::delay_ms(POLL_INTERVAL_MS);
                continue;
            }
            _ => {}
        }

        // --- 何かかざされた: F (交通系IDm、日常の主役) → A (HCE/UID) → B (免許証) ---
        // 軽い単発交換 (F/A の検出は数ms) を先に、重い APDU セッション (B) を
        // 最後に試す。主要経路の交通系タップが最速になる並び
        let mut got = false;

        match poll_felica_idm() {
            Ok(Some(idm)) => {
                if last_idm.as_deref() != Some(idm.as_str()) {
                    // push_event はイベントログ (UI/WS) 行のみで serial には出ない。
                    // log::info! を並置して scripts/nfc_serial_beep.py (COM 監視、
                    // 既定 --match "NFC|免許|IDm") で検知音を鳴らせるようにする (issue #101)
                    log::info!("NFC IDm={idm}");
                    push_event(&status, &format!("NFC IDm={idm}"));
                    beep_ok(&mut speaker);
                }
                last_idm = Some(idm);
                got = true;
            }
            Ok(None) => last_idm = None,
            Err(e) => log::warn!("nfc: FeliCa poll error: {e:#}"),
        }

        if !got {
            match poll_nfca_uid() {
                Ok(Some(uid)) => {
                    if last_uid.as_deref() != Some(uid.as_str()) {
                        log::info!("NFC-A UID={uid}");
                        push_event(&status, &format!("NFC-A UID={uid}"));
                        beep_ok(&mut speaker);
                    }
                    last_uid = Some(uid);
                    got = true;
                }
                Ok(None) => last_uid = None,
                Err(e) => log::warn!("nfc: NFC-A poll error: {e:#}"),
            }
        }

        if !got {
            let (rc, issue, expiry) = read_license_expiry();
            if rc == 0 {
                if last_license_rc != 0 {
                    log::info!("免許証 交付 {issue} 期限 {expiry}");
                    push_event(&status, &format!("免許証 交付 {issue} 期限 {expiry}"));
                    beep_ok(&mut speaker);
                }
                got = true;
            } else if rc != -2 && rc != last_license_rc {
                // 途中死はカード引き抜き等でも出る
                log::warn!("nfc: 免許証 読み取り失敗 rc={rc} ({})", license_rc_reason(rc));
            }
            last_license_rc = rc;
        }

        if got {
            triggered_since = None;
        }

        FreeRtos::delay_ms(POLL_INTERVAL_MS);
    }
}

/// 検知成功ビープ (issue #101 PR2)。2kHz 100ms 矩形波。I2S write は
/// ブロッキングだが 100ms 程度ならポーリング間隔への影響は許容範囲
fn beep_ok(speaker: &mut Speaker) {
    if let Err(e) = speaker.beep(2000.0, 100) {
        log::warn!("nfc: beep failed: {e:#}");
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

fn poll_nfca_uid() -> Result<Option<String>> {
    let mut buf = [0u8; 32];
    let n = unsafe { nfc_shim_poll_nfca_uid(buf.as_mut_ptr(), buf.len() as i32) };
    if n == 0 {
        return Ok(None);
    }
    if n < 0 {
        bail!("nfc_shim_poll_nfca_uid rc={n}");
    }
    Ok(Some(
        String::from_utf8_lossy(&buf[..n as usize]).into_owned(),
    ))
}

/// 従来 IC 運転免許証の PIN なし有効期限読み取り (EF 2F01)。戻り値は
/// (rc, 交付日, 有効期限)。rc==0 のときのみ日付が有効
fn read_license_expiry() -> (i32, String, String) {
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
        return (rc, String::new(), String::new());
    }
    (rc, cstr_bytes_to_str(&issue), cstr_bytes_to_str(&expiry))
}

fn cstr_bytes_to_str(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// components/nfc_shim/nfc_shim.cpp の nfc_shim_read_license_expiry() コメント準拠
fn license_rc_reason(rc: i32) -> &'static str {
    match rc {
        0 => "OK",
        -1 => "初期化未完了 or バッファ不足",
        -2 => "カード無し",
        -3 => "ATTRIB 失敗",
        -4 => "SELECT MF 失敗 (免許証以外の Type-B カードの可能性)",
        -5 => "SELECT EF 2F01 失敗",
        -6 => "READ BINARY 失敗",
        -7 => "データ長が想定より短い (EF 長が事前想定と違う、実機で要再調整)",
        _ => "不明なエラーコード",
    }
}

fn push_event(status: &SharedStatus, line: &str) {
    if let Ok(mut st) = status.lock() {
        st.push_event(now_ms(), line);
    }
}
