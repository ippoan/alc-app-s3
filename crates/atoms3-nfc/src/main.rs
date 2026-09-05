//! alc-hub-atoms3-nfc: AtomS3 Lite + Unit NFC (ST25R3916) ベンチ検証機。
//!
//! CoreS3 統合ハブから NFC 検証だけを切り出した独立ファームウェア
//! (issue #84 / plan/nfc-card-identity.md)。CoreS3 側は LAN/RS232 モジュール
//! 併用時、内蔵スピーカー(I2S DATA_OUT=固定 G13) と LAN CS ジャンパ
//! (G5=G1 / G15=G13、G1 は RS232M 自身の CS と衝突) が逃げ場なく競合するため
//! (plan/cores3-hub-consolidation.md 参照)、LAN/RS232 非搭載の AtomS3 Lite へ
//! NFC 検証を移設した。
//!
//! # 読み取りループは持たない (issue #146)
//!
//! **NFC の読み取り・重複抑止は `alc_hub_drivers::nfc` が正本。**本 crate は
//! そこへピンを渡し、通知 (LED) を受けるだけにする。以前はこのファイルが
//! 独自のポーリングループと「直前に読めた ID と違えば発火 / 読めなければ直前値を
//! クリア」の旧エッジ判定を持っていた。その判定こそ issue #103 で
//! 「1 タップ 2 重読み」の原因と特定したもので、
//! **検証機で測ると本番機 (crates/atoms3-timecard) と違う挙動が出る**
//! (issue #143 の実機計測がこれで成立しなかった)。検証機と本番機で
//! 重複抑止が同じ実装であることが、この crate の存在意義の前提になる。
//!
//! 通知は PC 側 `scripts/nfc_serial_beep.py` がシリアルログ (hub-drivers/nfc.rs
//! が出す `NFC IDm=…` / `免許証 交付 …`) を監視してビープを鳴らす方式に加え、
//! 本体 LED (WS2812) でもカード検知時に色を変える。待受中は暗い青 (生存確認)、
//! 検知成功 (IDm/免許証) は緑、読み取り失敗とカード 2 枚 (#143) は赤。
//!
//! **打刻は送らない。** WS/HTTP の uplink を持たないベンチ専用機なので、
//! ここでカードを読んでもサーバには何も届かない。
//!
//! 配線: Grove Port A (SDA=G2 / SCL=G1)。nfc_shim 側が I2C バスを自前で
//! 立てるため、Rust 側で `Peripherals::take()` は LED (RMT + GPIO35) と
//! ピン番号の受け渡しにのみ使う。

// esp-idf-hal 0.46 の legacy RMT は deprecated 扱いだが、新 RMT API は ws2812 の
// ビット列を組む口が無く、また新旧が同一バイナリに載ると
// check_rmt_legacy_driver_conflict で abort する (sdkconfig.defaults 参照)。
// crates/atoms3-timecard/src/led.rs と同じ理由・同じ抑止
#![allow(deprecated)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alc_hub_common::status::{HubStatus, SharedStatus};
use alc_hub_drivers::nfc::{self, NfcEvent};
use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::rmt::{
    config::TransmitConfig, FixedLengthSignal, PinState, Pulse, TxRmtDriver,
};

/// nfc_shim (C++ 側) に立てさせる I2C ポート。本機は他に I2C を使わないので
/// I2C_NUM_0 (実機確認済み 2026-07-21)。CoreS3 は内部バスが I2C_NUM_0 を
/// 使うので向こうは 1。abort していた原因 ("CONFLICT! driver_ng is not allowed
/// to be used with this old driver") は sdkconfig.defaults の
/// CONFIG_I2C_SKIP_LEGACY_CONFLICT_CHECK=y で解消済み
const I2C_PORT_NFC: i32 = 0;

// デバッグのため一時的にかなり明るくして「見えているか」自体を確認する
// (元は暗め (0,0,8) だったが実機で無点灯と報告あり、2026-07-20)
const LED_IDLE: (u8, u8, u8) = (0, 0, 255);
const LED_OK: (u8, u8, u8) = (0, 255, 0);
const LED_ERR: (u8, u8, u8) = (255, 0, 0);

/// 検知色を維持する時間。RF リンクは per-exchange で確率的に落ちるため、
/// 成功直後の一時的な失敗で表示を戻すと「不安定」に見える (issue #96)
const LATCH: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("alc-hub-atoms3-nfc 起動 (Unit NFC 検証、Port A: SDA=G2/SCL=G1)");
    log::info!("firmware build time: {}", env!("FIRMWARE_BUILD_TIME"));

    let p = Peripherals::take()?;
    // AtomS3 Lite 本体 LED (WS2812)。GPIO38 という情報は Web 検索の要約のみで
    // 未検証だった — 無点灯の実機報告を受け M5Unified 公式ボード定義
    // (_pin_table_other0, "//RGBLED" コメント付き) を確認したところ実際は
    // GPIO35 だった (2026-07-20)。legacy RMT ドライバで直接ビットバンギング
    // (ws2812-esp32-rmt-driver crate は esp-idf-hal 0.46 と links 衝突するため不使用)
    let tx = TxRmtDriver::new(
        p.rmt.channel0,
        p.pins.gpio35,
        &TransmitConfig::new().clock_divider(1),
    )?;
    let led = Arc::new(Mutex::new(Led::new(tx)));

    // 本機は画面もホストリンクも持たないので、push_event の行き先は捨て場。
    // それでも `nfc::start` はボード非依存の口として status を要求する
    let status: SharedStatus = Arc::new(Mutex::new(HubStatus::default()));

    // Unit NFC (ST25R3916): Grove Port A (SDA=G2 / SCL=G1)。読み取りループと
    // 重複抑止 (TapGate) は hub-drivers/src/nfc.rs が持つ。
    // **ここに NFC のコードを書かないこと** (issue #146)
    let led_for_nfc = Arc::clone(&led);
    nfc::start(
        I2C_PORT_NFC,
        p.pins.gpio2.into(),
        p.pins.gpio1.into(),
        Arc::clone(&status),
        move |e: &NfcEvent| paint_event(&led_for_nfc, e),
    )?;

    // メインループは LED のラッチ戻しだけ。検知そのもののログは nfc.rs が出す
    loop {
        FreeRtos::delay_ms(50);
        if let Ok(mut led) = led.lock() {
            led.expire();
        }
    }
}

/// 検知結果を LED の色にする。**2 枚見え (#143) と読み取り失敗は赤** —
/// どちらも「かざしたのに登録されなかった」ことを目視で分ける必要がある
fn paint_event(led: &Mutex<Led>, event: &NfcEvent) {
    let color = match event {
        NfcEvent::ReadFailed { .. } | NfcEvent::MultipleCards => LED_ERR,
        NfcEvent::Felica { .. }
        | NfcEvent::NfcaUid { .. }
        | NfcEvent::CarInspection { .. }
        | NfcEvent::License { .. } => LED_OK,
    };
    if let Ok(mut led) = led.lock() {
        led.paint(color);
    }
}

/// 本体 LED (WS2812) とラッチ状態。**塗るのは NFC スレッド (sink クロージャ)、
/// 待機色へ戻すのは main ループ**なので Mutex 越しに共有する。
/// crates/atoms3-timecard の `led.rs` とは共有しない — ボードは同じでも
/// あちらは打刻端末の運用表示、こちらはベンチの目視デバッグで寿命が違う
struct Led {
    tx: TxRmtDriver<'static>,
    /// 現在出している色 (同色の再送出を避ける)
    shown: (u8, u8, u8),
    since: Instant,
}

impl Led {
    fn new(tx: TxRmtDriver<'static>) -> Self {
        let mut led = Self {
            tx,
            // 待機色以外にしておき、初回 paint で必ず 1 回描かせる
            shown: LED_OK,
            since: Instant::now(),
        };
        led.paint(LED_IDLE);
        led
    }

    fn paint(&mut self, color: (u8, u8, u8)) {
        self.since = Instant::now();
        if self.shown == color {
            return;
        }
        self.shown = color;
        if let Err(e) = write_ws2812(&mut self.tx, color) {
            log::warn!("led: write failed: {e:#}");
        }
    }

    /// ラッチを過ぎていれば待機色へ戻す
    fn expire(&mut self) {
        if self.shown != LED_IDLE && self.since.elapsed() > LATCH {
            self.paint(LED_IDLE);
        }
    }
}

/// WS2812 へ 1 ピクセル分の (R,G,B) を送る (esp-idf-hal 公式 rmt_neopixel 例に準拠)。
/// GRB 順で 24bit を MSB から送出する
fn write_ws2812(tx: &mut TxRmtDriver<'static>, (r, g, b): (u8, u8, u8)) -> Result<()> {
    let color: u32 = ((g as u32) << 16) | ((r as u32) << 8) | b as u32;
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
}
