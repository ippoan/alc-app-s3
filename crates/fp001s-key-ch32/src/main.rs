//! FP001S 端末 t4 (ippoan/vein-base pcb/fp001s_test_board) の鍵専用 CH32V203F8U6。
//!
//! USB CDC 1 本のコンソール (docs/console-protocol.md) で `PING` / `DEVICE` / `AUTH KEYGEN [FORCE]` /
//! `AUTH PUBKEY` / `AUTH SIGN <nonce>` だけに答える。応答の形は alc-hub-core の `alarm_key` と同じ
//! (crates/alarm-key-nostd、fp001s-rp2354 と共有)。ほかは `ERR UNSUPPORTED (fp001s-key)`。
//!
//! 認可: t3・alarm と同じく、届くのはローカルの USB だけで認可は無い (2026-11-18 ユーザー決定)。署名するのは
//! 小文字 hex 32 桁の nonce だけ、鍵が在れば `AUTH KEYGEN` は `FORCE` 無しで断る。
//! シリアル OTA は持たない (更新は SWD / ISP、README.md)。
//!
//! 基板: USB は PA11 = DM・PA12 = DP の直結 (D+ のプルアップは内蔵、CH334F のポート 3)、HSE は 12 MHz の水晶。
//! ほかの GPIO はどこにもつながっていない。

#![no_std]
#![no_main]

mod key;
mod noise;

use alarm_key_nostd::{self as ak, Cmd};
use ch32_hal as hal;
use embassy_futures::join::join;
use embassy_usb::class::cdc_acm::device::{CdcAcmClass, Config as CdcAcmConfig, State};
use embassy_usb::driver::EndpointError;
use hal::adc::Adc;
use hal::peripherals::{ADC1, USBD};
use hal::rcc::{AHBPrescaler, APBPrescaler, Hse, HseMode, HsPllSource, LsConfig, Pll, PllMul, PllPreDiv, PllSource, Sysclk};
use hal::time::Hertz;
use hal::usbd::Driver;
use hal::{bind_interrupts, usbd};
use panic_halt as _;

const KIND: &str = "fp001s-key";
const FLAVOR: &str = "fp001s-key";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const ERR_ENTROPY: &str = "ERR AUTH: entropy";

bind_interrupts!(struct Irqs {
    USB_LP_CAN1_RX0 => usbd::InterruptHandler<USBD>;
});

/// HSE 12 MHz x 8 = 96 MHz (SYSCLK); USB 96 / 2 = 48 MHz (ch32-hal picks USBPRE from the PLL's output).
/// The HSI is not accurate enough for USB full speed.
fn rcc() -> hal::rcc::Config {
    hal::rcc::Config {
        hse: Some(Hse { freq: Hertz(12_000_000), mode: HseMode::Oscillator }),
        sys: Sysclk::PLL,
        pll_src: PllSource::HSE,
        pll: Some(Pll { prediv: PllPreDiv::DIV1, mul: PllMul::MUL8 }),
        pllx: None,
        ahb_pre: AHBPrescaler::DIV1,
        apb1_pre: APBPrescaler::DIV1,
        apb2_pre: APBPrescaler::DIV1,
        ls: LsConfig::default_lsi(),
        hspll_src: HsPllSource::HSE,
        hspll: None,
    }
}

type Class<'d> = CdcAcmClass<'d, Driver<'d, USBD>>;

struct Console {
    flash: key::KeyFlash,
    adc: Adc<'static, ADC1>,
    seed: Option<[u8; 32]>,
}

impl Console {
    fn reply(&mut self, line: &str) -> ak::Line {
        match ak::parse(line) {
            Cmd::Ping => ak::line("PONG"),
            Cmd::Device => ak::device_line(KIND, VERSION, FLAVOR),
            Cmd::Keygen { force } => {
                if self.seed.is_some() && !force {
                    ak::line(ak::ERR_KEY_EXISTS)
                } else {
                    match noise::seed(&mut self.adc) {
                        Err(_) => ak::line(ERR_ENTROPY),
                        Ok(s) if key::store(&mut self.flash, &s) => {
                            self.seed = Some(s);
                            ak::pubkey_line(&s)
                        }
                        Ok(_) => ak::line(ak::ERR_FLASH),
                    }
                }
            }
            Cmd::Pubkey => match &self.seed {
                Some(s) => ak::pubkey_line(s),
                None => ak::line(ak::ERR_NO_KEY),
            },
            Cmd::Sign(nonce) => match &self.seed {
                None => ak::line(ak::ERR_NO_KEY),
                Some(s) => ak::sig_line(s, nonce).unwrap_or_else(|| ak::line(ak::ERR_BAD_NONCE)),
            },
            Cmd::Other => ak::unsupported_line(KIND),
        }
    }
}

async fn console(class: &mut Class<'_>, con: &mut Console) -> ! {
    let mut line: heapless::String<128> = heapless::String::new();
    loop {
        class.wait_connection().await;
        let mut buf = [0u8; 64];
        'conn: loop {
            let Ok(n) = class.read_packet(&mut buf).await else { break 'conn };
            for &b in &buf[..n] {
                if b == b'\r' || b == b'\n' {
                    if line.is_empty() {
                        continue;
                    }
                    let reply = con.reply(line.as_str());
                    line.clear();
                    if write_line(class, &reply).await.is_err() {
                        break 'conn;
                    }
                } else if line.push(b as char).is_err() {
                    line.clear(); // over-long garbage: drop it
                }
            }
        }
        line.clear();
    }
}

/// `\r\n<line>\r\n` in 64-byte packets, a zero-length packet when the last one is full (as fp001s-rp2354)
async fn write_line(class: &mut Class<'_>, s: &str) -> Result<(), EndpointError> {
    let mut out: heapless::Vec<u8, 200> = heapless::Vec::new();
    let _ = out.extend_from_slice(b"\r\n");
    let _ = out.extend_from_slice(s.as_bytes());
    let _ = out.extend_from_slice(b"\r\n");
    for chunk in out.chunks(64) {
        class.write_packet(chunk).await?;
    }
    if out.len() % 64 == 0 {
        class.write_packet(&[]).await?;
    }
    Ok(())
}

#[embassy_executor::main(entry = "qingke_rt::entry")]
async fn main(_spawner: embassy_executor::Spawner) {
    let p = hal::init(hal::Config { rcc: rcc(), ..Default::default() });

    let driver = Driver::new(p.USBD, Irqs, p.PA12, p.PA11);
    let mut config = embassy_usb::Config::new(0x1209, 0x0001); // pid.codes' test PID (TODO: our own)
    config.manufacturer = Some("ippoan");
    config.product = Some("FP001S key");
    config.serial_number = Some("fp001s-key");
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    config.device_class = 0x02;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x00;
    config.composite_with_iads = false;

    let mut config_desc = [0u8; 128];
    let mut bos_desc = [0u8; 32];
    let mut control_buf = [0u8; 64];
    let mut state = State::new();
    let mut builder = embassy_usb::Builder::new(driver, config, &mut config_desc, &mut bos_desc, &mut [], &mut control_buf);
    let mut class = CdcAcmClass::new(&mut builder, &mut state, CdcAcmConfig::new());
    let mut usb = builder.build();

    let mut flash = key::KeyFlash::new_blocking(p.FLASH);
    let seed = key::load(&mut flash);
    // the ADC's clock: PCLK2 (96 MHz) / 8 = 12 MHz, under its 14 MHz (ch32-hal leaves ADCPRE at its reset /2)
    hal::pac::RCC.cfgr0().modify(|w| w.set_adcpre(hal::pac::rcc::vals::Adcpre::DIV8));
    let adc = Adc::new(p.ADC1, Default::default());
    let mut con = Console { flash, adc, seed };

    join(usb.run(), console(&mut class, &mut con)).await;
}
