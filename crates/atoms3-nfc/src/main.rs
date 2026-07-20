//! alc-hub-atoms3-nfc: AtomS3 Lite + Unit NFC (ST25R3916) ベンチ検証機。
//!
//! CoreS3 統合ハブの `alc_hub_drivers::nfc` から NFC 検証だけを切り出した
//! 独立ファームウェア (issue #84 / plan/nfc-card-identity.md)。CoreS3 側は
//! LAN/RS232 モジュール併用時、内蔵スピーカー(I2S DATA_OUT=固定 G13) と
//! LAN CS ジャンパ(G5=G1 / G15=G13、G1 は RS232M 自身の CS と衝突)が
//! 逃げ場なく競合するため (plan/cores3-hub-consolidation.md 参照)、
//! LAN/RS232 非搭載の AtomS3 Lite へ NFC 検証を移設した。
//!
//! 通知は PC 側 `scripts/nfc_serial_beep.py` がシリアルログを監視してビープを
//! 鳴らす方式に加え、本体 LED (WS2812, GPIO38) でもカード検知時に色を変える。
//! 待受中は暗い青 (生存確認)、検知成功 (IDm/免許証) は緑、免許証読み取り失敗
//! (カードは反応したがエラー) は赤。
//!
//! 配線: Grove Port A (SDA=G1 / SCL=G2)。nfc_shim 側が I2C バスを自前で
//! 立てるため、Rust 側で `Peripherals::take()` は LED (RMT + GPIO38) の
//! 予約にのみ使う (I2C ピンは nfc_shim に渡す番号だけで良い、hub-drivers/nfc.rs
//! と同じ設計)。

use std::time::Duration;

use anyhow::{bail, Result};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::rmt::{config::TransmitConfig, FixedLengthSignal, PinState, Pulse, TxRmtDriver};

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
}

/// true の間、F/A ポーリングをスキップして NFC-B (免許証) だけに専念させ、
/// read_license_expiry() の rc を毎回ログする (2026-07-20、issue #96 の
/// 本番経路切り分け用。原因判明後に false に戻すか削除する)
const SKIP_FA_POLL_FOR_B_DIAGNOSIS: bool = true;

/// 2026-07-20 時点では I2C_NUM_1 だったが、issue #96 の切り分けで
/// I2C_NUM_0 (動作確認済みの診断コードが M5Unified wiring::addI2C 経由で実際
/// に使っているポート) に一時的に戻して差を検証する。abort していた原因
/// ("CONFLICT! driver_ng is not allowed to be used with this old driver") は
/// sdkconfig.defaults の CONFIG_I2C_SKIP_LEGACY_CONFLICT_CHECK=y で既に
/// 解消済みのはずなので、I2C_NUM_0 でも abort しない見込み
const I2C_PORT_NFC: i32 = 0;
/// Grove Port A (AtomS3 Lite 公式ピンマップ: SDA=G1 / SCL=G2)。
/// rc=-3 (g_units.begin() 失敗 = デバイス無応答) を実機で確認 (2026-07-20)。
/// 配線は正しいとのことなので、まず SDA/SCL 入れ替えを試す (CoreS3 側
/// hub-drivers/nfc.rs のコメントにも同じ ack 無し時の対処が残っている)
const PIN_SDA: i32 = 2;
const PIN_SCL: i32 = 1;

const POLL_INTERVAL_MS: u32 = 200;

// デバッグのため一時的にかなり明るくして「見えているか」自体を確認する
// (元は暗め (0,0,8) だったが実機で無点灯と報告あり、2026-07-20)
const LED_IDLE: (u8, u8, u8) = (0, 0, 255);
const LED_OK: (u8, u8, u8) = (0, 255, 0);
const LED_ERR: (u8, u8, u8) = (255, 0, 0);

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("alc-hub-atoms3-nfc 起動 (Unit NFC 検証、Port A: SDA=G1/SCL=G2)");
    log::info!("firmware build time: {}", env!("FIRMWARE_BUILD_TIME"));

    let p = Peripherals::take()?;
    // AtomS3 Lite 本体 LED (WS2812)。GPIO38 という情報は Web 検索の要約のみで
    // 未検証だった — 無点灯の実機報告を受け M5Unified 公式ボード定義
    // (_pin_table_other0, "//RGBLED" コメント付き) を確認したところ実際は
    // GPIO35 だった (2026-07-20)。legacy RMT ドライバで直接ビットバンギング
    // (ws2812-esp32-rmt-driver crate は esp-idf-hal 0.46 と links 衝突するため不使用)
    let mut led = TxRmtDriver::new(p.rmt.channel0, p.pins.gpio35, &TransmitConfig::new().clock_divider(1))?;
    set_led(&mut led, LED_IDLE);

    let rc = unsafe { nfc_shim_init(I2C_PORT_NFC, PIN_SDA, PIN_SCL) };
    if rc != 0 {
        log::error!("NFC 初期化失敗 rc={rc} (Unit NFC の配線/電源を確認)");
        set_led(&mut led, LED_ERR);
        loop {
            FreeRtos::delay_ms(1000);
        }
    }
    log::info!("NFC 待受開始");

    let mut last_idm: Option<String> = None;
    let mut last_uid: Option<String> = None;
    // -2 (カード無し) はループの大半で発生する定常状態なのでログしない。
    // 未実行を表すセンチネルとして i32::MIN を使う
    let mut last_license_rc = i32::MIN;

    // デバッグ用: 検出試行が実際に走っているか確認するためのハートビート
    // (無反応の実機報告を受け一時追加、2026-07-20。原因判明後に削除予定)
    let mut tick: u32 = 0;

    loop {
        // F/A ポーリングは一時的にスキップ (2026-07-20): 診断用サンプルテストで
        // NFC-B は F/A を挟まず専念させたときに確実に検出できた実績があり、
        // 本番ループの F→A→B 周回が NFC-B の検出を妨げていないか切り分け中
        if !SKIP_FA_POLL_FOR_B_DIAGNOSIS {
            match poll_felica_idm() {
                Ok(Some(idm)) => {
                    if last_idm.as_deref() != Some(idm.as_str()) {
                        log::info!("NFC IDm={idm}");
                        last_idm = Some(idm);
                    }
                    set_led(&mut led, LED_OK);
                }
                Ok(None) => {
                    if last_idm.is_some() {
                        set_led(&mut led, LED_IDLE);
                    }
                    last_idm = None;
                }
                Err(e) => log::warn!("nfc: FeliCa poll error: {e:#}"),
            }
            FreeRtos::delay_ms(POLL_INTERVAL_MS);

            // NTAG213 等 Type-A の既知良品カードでの切り分け用 (2026-07-20 追加)
            match poll_nfca_uid() {
                Ok(Some(uid)) => {
                    if last_uid.as_deref() != Some(uid.as_str()) {
                        log::info!("NFC-A UID={uid}");
                        last_uid = Some(uid);
                    }
                    set_led(&mut led, LED_OK);
                }
                Ok(None) => last_uid = None,
                Err(e) => log::warn!("nfc: NFC-A poll error: {e:#}"),
            }
            FreeRtos::delay_ms(POLL_INTERVAL_MS);
        }

        let (rc, issue, expiry) = read_license_expiry();
        // 診断中は rc を毎回ログする (通常は -2=カード無し が定常状態なので
        // 変化時のみログしていたが、どのステップで失敗しているか見るため)
        if SKIP_FA_POLL_FOR_B_DIAGNOSIS {
            log::info!("read_license_expiry rc={rc} ({})", license_rc_reason(rc));
        }
        if rc != -2 {
            if rc != last_license_rc {
                if rc == 0 {
                    log::info!("免許証 交付 {issue} 期限 {expiry}");
                } else {
                    log::warn!("免許証 読み取り失敗 rc={rc} ({})", license_rc_reason(rc));
                }
            }
            set_led(&mut led, if rc == 0 { LED_OK } else { LED_ERR });
        } else if last_license_rc != -2 && last_idm.is_none() {
            // 免許証も交通系IDmも今は無し = 待受アイドルへ戻す
            set_led(&mut led, LED_IDLE);
        }
        last_license_rc = rc;

        tick += 1;
        if tick % 10 == 0 {
            log::info!(
                "heartbeat tick={tick} idm={last_idm:?} uid={last_uid:?} license_rc={last_license_rc}"
            );
        }
        FreeRtos::delay_ms(POLL_INTERVAL_MS);
    }
}

/// WS2812 へ 1 ピクセル分の (R,G,B) を送る (esp-idf-hal 公式 rmt_neopixel 例に準拠)。
/// GRB 順で 24bit を MSB から送出する
fn set_led(tx: &mut TxRmtDriver, (r, g, b): (u8, u8, u8)) {
    let color: u32 = ((g as u32) << 16) | ((r as u32) << 8) | b as u32;
    let res: Result<()> = (|| {
        let ticks_hz = tx.counter_clock()?;
        let t0h = Pulse::new_with_duration(ticks_hz, PinState::High, &Duration::from_nanos(350))?;
        let t0l = Pulse::new_with_duration(ticks_hz, PinState::Low, &Duration::from_nanos(800))?;
        let t1h = Pulse::new_with_duration(ticks_hz, PinState::High, &Duration::from_nanos(700))?;
        let t1l = Pulse::new_with_duration(ticks_hz, PinState::Low, &Duration::from_nanos(600))?;
        let mut signal = FixedLengthSignal::<24>::new();
        for i in (0..24u32).rev() {
            let bit = (color >> i) & 1 != 0;
            let (high, low) = if bit { (t1h, t1l) } else { (t0h, t0l) };
            signal.set(23 - i as usize, &(high, low))?;
        }
        tx.start_blocking(&signal)?;
        Ok(())
    })();
    if let Err(e) = res {
        log::warn!("led: write failed: {e:#}");
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
