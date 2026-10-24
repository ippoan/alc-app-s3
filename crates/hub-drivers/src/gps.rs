//! Unit GPS (AT6558 / AT6668) の UART 読み (Vein Base の Grove PORT.A、`vein-gps`、
//! ippoan/alc-app#514)。
//!
//! 読むだけの専用スレッドを 1 本立て、`EVT GPS …` の行を USB シリアルへ出す。
//! 行を出す規則 (最初の NMEA が来るまで黙る / FIX・NOFIX の変化と 30 秒ごと) と
//! RX ピンの当て方 (G1 を先に試し、3 秒来なければ G2) は
//! [`alc_hub_core::gps_report`] (host test 付き) が持つ。NMEA の解釈は [`alc_hub_core::gps`]。
//!
//! UART0 / 115200bps 固定 (Unit GPS v1.1 = AT6668)、RX のみ。ボーレートの自動判別はしない。
//! ピンの差し替えは `uart_set_pin` で RX だけ替える。TX 用に渡された G2 は最初に
//! GPIO へ戻して出力を切る (RX 候補と同じピンなので、出しっぱなしだと GPS の TX と衝突する)。
//!
//! **1 行は `println!` 1 回で出す** — シリアルの行は他のログに割り込まれるため。

use std::time::Instant;

use anyhow::Result;
use esp_idf_svc::hal::{
    delay::TickType,
    gpio::{AnyIOPin, Pin},
    uart::{config::Config as UartConfig, Uart, UartDriver},
    units::Hertz,
};
use esp_idf_svc::sys::{gpio_reset_pin, uart_set_pin, UART_PIN_NO_CHANGE};

use alc_hub_core::gps_report::{Reporter, RxPin, RxProbe};

/// Unit GPS の UART 速度
pub const BAUD: u32 = 115_200;

/// 読みスレッドを立てる。`g1` / `g2` は Grove PORT.A の黄 / 白 (G1 / G2)
pub fn start(
    uart: impl Uart + 'static,
    g1: AnyIOPin<'static>,
    g2: AnyIOPin<'static>,
) -> Result<()> {
    let (n1, n2) = (i32::from(g1.pin()), i32::from(g2.pin()));
    // TX は使わないが UartDriver::new が要るので G2 を渡し、直後に GPIO へ戻す
    let driver = UartDriver::new(
        uart,
        g2,
        g1,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &UartConfig::new().baudrate(Hertz(BAUD)),
    )?;
    // SAFETY: 生きている UART のピンを GPIO に戻すだけ
    unsafe { gpio_reset_pin(n2) };
    std::thread::Builder::new()
        .name("gps".into())
        .stack_size(4 * 1024)
        .spawn(move || run(driver, n1, n2))?;
    Ok(())
}

fn run(driver: UartDriver<'static>, n1: i32, n2: i32) {
    let t0 = Instant::now();
    let now_ms = || t0.elapsed().as_millis() as u64;
    let mut reporter = Reporter::default();
    let mut probe = RxProbe::new(0);
    let mut buf = [0u8; 128];
    loop {
        let n = driver
            .read(&mut buf, TickType::new_millis(50).ticks())
            .unwrap_or(0);
        let mut verified = false;
        for &b in &buf[..n] {
            let (ok, line) = reporter.push(b, now_ms());
            verified |= ok;
            if let Some(line) = line {
                println!("{line}");
            }
        }
        if let Some(pin) = probe.observe(now_ms(), verified) {
            let rx = if pin == RxPin::G1 { n1 } else { n2 };
            // SAFETY: ドライバを入れた UART の RX だけを差し替える (ドライバは生きている)
            let r = unsafe {
                uart_set_pin(
                    driver.port(),
                    UART_PIN_NO_CHANGE,
                    rx,
                    UART_PIN_NO_CHANGE,
                    UART_PIN_NO_CHANGE,
                )
            };
            if r != 0 {
                log::warn!("gps: uart_set_pin rx={rx} failed ({r})");
            }
            let _ = driver.clear_rx();
        }
    }
}
