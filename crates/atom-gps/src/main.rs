//! alc-hub-atom-gps: M5Stack ATOM (Lite / Matrix) + Unit GPS のベンチ検証機。
//!
//! Unit GPS が UART に流す NMEA を読み、1 秒ごとに測位結果を 1 行で USB
//! シリアルへ出す:
//!
//! ```text
//! EVT GPS UART rx=G32 tx=G26 baud=9600
//! EVT GPS NOFIX sats=3 utc=2026-10-24T06:30:00Z
//! EVT GPS FIX lat=35.681236 lon=139.767125 sats=8 hdop=0.9 alt=40.1 utc=2026-10-24T06:30:00Z
//! ```
//!
//! **NMEA の解釈は `alc_hub_core::gps` が正本**で、この crate は UART の配線だけを
//! 持つ (ホストでテストできる部分を firmware 側に書かない)。
//!
//! # 配線とボーレートは自動で当てる
//!
//! ATOM の Grove は G26 / G32 の 2 本で、ケーブルの向き (どちらが Unit GPS の TX か)
//! と Unit の版 (v1.0 = AT6558 は 9600、v1.1 = AT6668 は 115200) で組み合わせが
//! 変わる。起動時と、NMEA が [`LOST_AFTER`] 途切れたときに [`CANDIDATES`] を
//! 順に試し、checksum の合う NMEA の行が読めた組で止まる。

use std::time::{Duration, Instant};

use alc_hub_core::gps::{verify, LineBuf, Tracker};
use anyhow::Result;
use esp_idf_svc::hal::delay::TickType;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::{config::Config as UartConfig, UartDriver};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::{uart_set_pin, UART_PIN_NO_CHANGE};

/// 試す (RX, TX, ボーレート) の組。先頭が M5Stack の ATOM 向けサンプルの配線
/// (`Serial2.begin(9600, SERIAL_8N1, 32, 26)`)
const CANDIDATES: [(i32, i32, u32); 6] = [
    (32, 26, 9600),
    (32, 26, 115_200),
    (26, 32, 9600),
    (26, 32, 115_200),
    (32, 26, 38_400),
    (26, 32, 38_400),
];

/// 1 つの組で NMEA を待つ時間。Unit GPS は 1 秒ごとに数行出すので 2 周期ぶん
const PROBE_FOR: Duration = Duration::from_millis(2500);
/// 読めていた NMEA がこれだけ途切れたら配線を当て直す
const LOST_AFTER: Duration = Duration::from_secs(5);
/// 測位結果を出す間隔
const REPORT_EVERY: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    println!("EVT GPS BOOT");

    let p = Peripherals::take()?;
    let uart = UartDriver::new(
        p.uart1,
        p.pins.gpio26,
        p.pins.gpio32,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &UartConfig::new().baudrate(Hertz(CANDIDATES[0].2)),
    )?;

    loop {
        probe(&uart)?;
        track(&uart);
        println!("EVT GPS LOST");
    }
}

/// NMEA が読める (RX, TX, ボーレート) の組を見つけるまで回る
fn probe(uart: &UartDriver) -> Result<()> {
    loop {
        for &(rx, tx, baud) in &CANDIDATES {
            // SAFETY: ドライバを入れた UART のピンを差し替えるだけ (ドライバは生きている)
            esp_idf_svc::sys::esp!(unsafe {
                uart_set_pin(uart.port(), tx, rx, UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE)
            })?;
            uart.change_baudrate(Hertz(baud))?;
            uart.clear_rx()?;
            if wait_nmea(uart) {
                println!("EVT GPS UART rx=G{rx} tx=G{tx} baud={baud}");
                return Ok(());
            }
        }
        println!("EVT GPS NO_DATA");
    }
}

/// [`PROBE_FOR`] のあいだに checksum の合う NMEA の行が 1 本でも読めたら `true`
fn wait_nmea(uart: &UartDriver) -> bool {
    let mut lines = LineBuf::default();
    let start = Instant::now();
    let mut buf = [0u8; 128];
    while start.elapsed() < PROBE_FOR {
        let n = uart
            .read(&mut buf, TickType::new_millis(50).ticks())
            .unwrap_or(0);
        for &b in &buf[..n] {
            if lines.push(b).is_some_and(|l| verify(&l).is_some()) {
                return true;
            }
        }
    }
    false
}

/// 測位結果を [`REPORT_EVERY`] ごとに出す。NMEA が [`LOST_AFTER`] 途切れたら戻る
fn track(uart: &UartDriver) {
    let mut lines = LineBuf::default();
    let mut tracker = Tracker::default();
    let mut last_nmea = Instant::now();
    let mut last_report = Instant::now();
    let mut buf = [0u8; 128];
    while last_nmea.elapsed() < LOST_AFTER {
        let n = uart
            .read(&mut buf, TickType::new_millis(50).ticks())
            .unwrap_or(0);
        for &b in &buf[..n] {
            if let Some(line) = lines.push(b) {
                if verify(&line).is_some() {
                    last_nmea = Instant::now();
                }
                tracker.feed(&line);
            }
        }
        if last_report.elapsed() >= REPORT_EVERY {
            last_report = Instant::now();
            println!("{}", tracker.fix().evt_line());
        }
    }
}
