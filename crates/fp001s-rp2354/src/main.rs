//! FP001S face / palm vein terminal: the RP2354A on ippoan/vein-base's pcb/fp001s_test_board (t3), or a Pico 2 wired
//! the same way (GP0 TX -> the module's RXD, GP1 RX <- its TXD).
//!
//! USB: one device, two CDC ACM ports (composite):
//!   - interface 0 = the console (docs/console-protocol.md: `PING`, `DEVICE`, `AUTH KEYGEN|PUBKEY|SIGN`)
//!   - interface 1 = the FP001S's UART as is (115200 8N1, the module's EF AA frames both ways, not parsed here)
//! The module's camera (UVC) is the other port of the board's hub, not this chip's.
//!
//! The device key (src/key.rs) is the VoiceS3R's way: made on the board, kept in flash, never sent out; no Secure
//! Boot, no OTP. The serial OTA (`OTA SERIAL` / `OTA CONFIRM`, A/B partitions with the boot ROM's try-before-you-buy)
//! is the next step.
#![no_std]
#![no_main]

mod key;
mod proto;

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_rp::bind_interrupts;
use embassy_rp::flash::Flash;
use embassy_rp::peripherals::{TRNG, UART0, USB};
use embassy_rp::trng::Trng;
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, BufferedUartRx, BufferedUartTx, Config as UartConfig};
use embassy_rp::usb::{Driver, InterruptHandler as UsbIrq};
use embassy_usb::class::cdc_acm::{CdcAcmClass, Receiver, Sender, State};
use embassy_usb::UsbDevice;
use embedded_io_async::{Read, Write};
use panic_halt as _;
use static_cell::StaticCell;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 3] = [
    embassy_rp::binary_info::rp_program_name!(c"alc-fp001s-rp2354"),
    embassy_rp::binary_info::rp_program_description!(c"FP001S terminal: USB console + FP001S UART bridge"),
    embassy_rp::binary_info::rp_cargo_version!(),
];

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbIrq<USB>;
    UART0_IRQ => BufferedInterruptHandler<UART0>;
    TRNG_IRQ => embassy_rp::trng::InterruptHandler<TRNG>;
});

type UsbDrv = Driver<'static, USB>;

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, UsbDrv>) -> ! {
    usb.run().await
}

/// interface 1 <-> UART0, byte for byte
#[embassy_executor::task]
async fn bridge_task(
    mut usb_tx: Sender<'static, UsbDrv>,
    mut usb_rx: Receiver<'static, UsbDrv>,
    mut uart_tx: BufferedUartTx,
    mut uart_rx: BufferedUartRx,
) -> ! {
    loop {
        usb_rx.wait_connection().await;
        let to_module = async {
            let mut buf = [0u8; 64];
            while let Ok(n) = usb_rx.read_packet(&mut buf).await {
                if uart_tx.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        };
        let from_module = async {
            let mut buf = [0u8; 64];
            loop {
                match uart_rx.read(&mut buf).await {
                    Ok(n) if n > 0 => {
                        if usb_tx.write_packet(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        };
        join(to_module, from_module).await;
    }
}

#[embassy_executor::task]
async fn console_task(mut class: CdcAcmClass<'static, UsbDrv>, mut flash: key::KeyFlash, mut trng: Trng<'static, TRNG>) -> ! {
    let mut seed = key::load(&mut flash);
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
                    let reply = match proto::parse(line.as_str()) {
                        proto::Cmd::Ping => proto::line("PONG"),
                        proto::Cmd::Device => proto::device_line(VERSION),
                        proto::Cmd::Keygen { force } => {
                            if seed.is_some() && !force {
                                proto::line("ERR AUTH: key exists")
                            } else {
                                let mut s = [0u8; 32];
                                trng.fill_bytes(&mut s).await;
                                if key::store(&mut flash, &s) {
                                    seed = Some(s);
                                    proto::pubkey_line(&s)
                                } else {
                                    proto::line("ERR AUTH: flash")
                                }
                            }
                        }
                        proto::Cmd::Pubkey => match &seed {
                            Some(s) => proto::pubkey_line(s),
                            None => proto::line("ERR AUTH: no key"),
                        },
                        proto::Cmd::Sign(nonce) => match &seed {
                            None => proto::line("ERR AUTH: no key"),
                            Some(s) => proto::sig_line(s, nonce).unwrap_or_else(|| proto::line("ERR AUTH: bad nonce")),
                        },
                        proto::Cmd::Unknown => proto::line("ERR UNSUPPORTED (fp001s)"),
                    };
                    line.clear();
                    // every reply starts on a fresh line and ends in CRLF (console-protocol.md §1)
                    if write_line(&mut class, reply.as_str()).await.is_err() {
                        break 'conn;
                    }
                } else if line.push(b as char).is_err() {
                    line.clear(); // over-long garbage: drop it
                }
            }
        }
    }
}

async fn write_line(class: &mut CdcAcmClass<'static, UsbDrv>, s: &str) -> Result<(), embassy_usb::driver::EndpointError> {
    let mut out: heapless::Vec<u8, 200> = heapless::Vec::new();
    let _ = out.extend_from_slice(b"\r\n");
    let _ = out.extend_from_slice(s.as_bytes());
    let _ = out.extend_from_slice(b"\r\n");
    for chunk in out.chunks(64) {
        class.write_packet(chunk).await?;
    }
    if out.len() % 64 == 0 {
        class.write_packet(&[]).await?; // a zero-length packet ends the transfer
    }
    Ok(())
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let driver = Driver::new(p.USB, Irqs);
    let mut config = embassy_usb::Config::new(0x2e8a, 0x000f); // Raspberry Pi's VID, RP2350's PID (TODO: our own)
    config.manufacturer = Some("ippoan");
    config.product = Some("FP001S terminal");
    config.serial_number = Some("fp001s");
    config.max_power = 500;
    config.max_packet_size_0 = 64;
    config.composite_with_iads = true;
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    let mut builder = embassy_usb::Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );
    static CONSOLE_STATE: StaticCell<State> = StaticCell::new();
    static BRIDGE_STATE: StaticCell<State> = StaticCell::new();
    let console = CdcAcmClass::new(&mut builder, CONSOLE_STATE.init(State::new()), 64);
    let bridge = CdcAcmClass::new(&mut builder, BRIDGE_STATE.init(State::new()), 64);
    let usb = builder.build();

    static TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
    let mut uc = UartConfig::default();
    uc.baudrate = 115_200;
    let uart = BufferedUart::new(p.UART0, p.PIN_0, p.PIN_1, Irqs, TX_BUF.init([0; 256]), RX_BUF.init([0; 1024]), uc);
    let (uart_tx, uart_rx) = uart.split();
    let (usb_tx, usb_rx) = bridge.split();

    let flash = Flash::new_blocking(p.FLASH);
    let trng = Trng::new(p.TRNG, Irqs, embassy_rp::trng::Config::default());

    spawner.spawn(usb_task(usb).unwrap());
    spawner.spawn(bridge_task(usb_tx, usb_rx, uart_tx, uart_rx).unwrap());
    spawner.spawn(console_task(console, flash, trng).unwrap());
}
