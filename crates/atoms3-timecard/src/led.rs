//! 本体 RGB LED (WS2812) — 待機 / 検知成功 / 読み取り失敗の 3 状態。
//!
//! **本番では LED を運用の手がかりにしない方針**なので凝らない。開発中に
//! 「かざしたのに何も起きていない」のか「読めたが送れていない」のかを
//! 手元で切り分けるためだけのもの (それはシリアルログでも分かる)。
//!
//! 色は crates/atoms3-nfc と同じ: 待機 = 暗い青 (生存確認) / 成功 = 緑 /
//! 失敗 = 赤。
//!
//! 描画は **main ループが 1 スレッドで行う**。NFC コールバックは
//! [`LedSignal`] にアトミックで色を置くだけ — RMT ドライバの所有権を
//! 1 スレッドに閉じ、かつ「検知後 1 秒で待機色へ戻す」ラッチを
//! ポーリング側に持たせるため。

// esp-idf-hal 0.46 の legacy RMT は deprecated 扱いだが、新 RMT API は
// ws2812 のビット列を組む口が無く、また新旧が同一バイナリに載ると
// check_rmt_legacy_driver_conflict で abort する (sdkconfig.defaults 参照)
#![allow(deprecated)]

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use esp_idf_hal::gpio::Gpio35;
use esp_idf_hal::rmt::{
    config::TransmitConfig, FixedLengthSignal, PinState, Pulse, TxRmtDriver, CHANNEL0,
};

/// 待機色。生存確認のための暗い青 (常時点灯なので明るくしない)
const IDLE: (u8, u8, u8) = (0, 0, 8);
/// 検知成功 (打刻イベントを送信キューへ積めた)
const OK: (u8, u8, u8) = (0, 255, 0);
/// 読み取り失敗 (カードは反応したが読めなかった)
const ERR: (u8, u8, u8) = (255, 0, 0);

/// 成功/失敗を出しっぱなしにする時間。RF リンクは per-exchange で確率的に
/// 落ちるため、直後の一時的な失敗で表示を戻すと「不安定」に見える (issue #96)
const LATCH: Duration = Duration::from_secs(1);

const STATE_IDLE: u8 = 0;
const STATE_OK: u8 = 1;
const STATE_ERR: u8 = 2;

/// NFC スレッドから main ループへ「次に出す色」を渡す口 (clone して渡す)
#[derive(Clone)]
pub struct LedSignal(Arc<AtomicU8>);

impl LedSignal {
    pub fn ok(&self) {
        self.0.store(STATE_OK, Ordering::Relaxed);
    }

    pub fn err(&self) {
        self.0.store(STATE_ERR, Ordering::Relaxed);
    }
}

/// LED ドライバ + ラッチ状態。main ループが [`Led::tick`] を定期的に呼ぶ
pub struct Led {
    tx: TxRmtDriver<'static>,
    signal: Arc<AtomicU8>,
    /// 現在出している色 (同色の再送出を避ける)
    shown: u8,
    since: Instant,
}

impl Led {
    /// AtomS3 Lite の本体 RGB LED は **G35** (M5Unified の
    /// `_pin_table_other0` の "//RGBLED" コメント付きエントリで確認、
    /// 2026-07-20)。Web 検索の要約で G38 と書いて実機で無点灯になった実害が
    /// atoms3-nfc にあるので、**機種を変えたら必ず公式定義を読み直すこと**
    pub fn new(channel: CHANNEL0<'static>, pin: Gpio35<'static>) -> Result<Self> {
        let tx = TxRmtDriver::new(channel, pin, &TransmitConfig::new().clock_divider(1))?;
        let mut led = Self {
            tx,
            signal: Arc::new(AtomicU8::new(STATE_IDLE)),
            // shown を IDLE 以外にしておき、最初の tick で必ず 1 回描かせる
            shown: u8::MAX,
            since: Instant::now(),
        };
        led.paint(STATE_IDLE);
        Ok(led)
    }

    pub fn signal(&self) -> LedSignal {
        LedSignal(Arc::clone(&self.signal))
    }

    /// 要求された色を反映し、ラッチを過ぎていれば待機色へ戻す
    pub fn tick(&mut self) {
        let requested = self.signal.swap(STATE_IDLE, Ordering::Relaxed);
        if requested != STATE_IDLE {
            self.paint(requested);
            return;
        }
        if self.shown != STATE_IDLE && self.since.elapsed() > LATCH {
            self.paint(STATE_IDLE);
        }
    }

    fn paint(&mut self, state: u8) {
        self.since = Instant::now();
        if self.shown == state {
            return;
        }
        self.shown = state;
        let color = match state {
            STATE_OK => OK,
            STATE_ERR => ERR,
            _ => IDLE,
        };
        if let Err(e) = write_ws2812(&mut self.tx, color) {
            log::warn!("led: write failed: {e:#}");
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
