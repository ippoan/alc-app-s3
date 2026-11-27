//! FP001S face / palm vein terminal: the RP2354A on ippoan/vein-base's pcb/fp001s_test_board (t3), or a Pico 2 wired
//! the same way (GP0 TX -> the module's RXD, GP1 RX <- its TXD).
//!
//! USB: one device, two CDC ACM ports (composite):
//!   - interface 0 = the console (docs/console-protocol.md: `PING`, `DEVICE`, `AUTH KEYGEN|PUBKEY|SIGN`,
//!     `OTA SERIAL|CONFIRM` (src/ota.rs))
//!   - interface 1 = the FP001S's UART as is (115200 8N1, the module's EF AA frames both ways, not parsed here)
//! FP008 only: its infrared proximity sensor's own serial port (manual ch. 7, the 4 pads by the 8-pin connector, 9600
//! 8N1, TX only is wired) on UART1 RX = GP5, and its IRQ (connector pin 5) on GP2; the console's `IR` reads both.
//! Presence: GP2 has a pull-up; once it has been seen low the module is an FP008 (`DEVICE ... BOARD=fp008`; the
//! FP001S has no IRQ line and the pin stays high). On an FP008 a rising edge starts a MID_VERIFY by itself (when
//! neither the host nor an earlier one is in flight) and its result comes out on the console as `EVT VERIFY ...`.
//! The module's camera (UVC) is the other port of the board's hub, not this chip's.
//!
//! The device key (src/key.rs) is the VoiceS3R's way: made on the board, kept in flash, never sent out; no Secure
//! Boot, no OTP.
#![no_std]
#![no_main]

mod key;
mod ota;
mod proto;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_rp::bind_interrupts;
use embassy_rp::flash::Flash;
use embassy_rp::gpio::{Input, Pull};
use embassy_rp::peripherals::{TRNG, UART0, UART1, USB};
use embassy_rp::watchdog::Watchdog;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use portable_atomic::{AtomicBool, AtomicU64, Ordering};
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

/// The ROM's IMAGE_DEF, try-before-you-buy (src/ota.rs): embassy-rp's own is off (feature imagedef-none).
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: embassy_rp::block::ImageDef = embassy_rp::block::ImageDef::new([embassy_rp::block::item_image_type_exe(
    embassy_rp::block::Security::Secure,
    embassy_rp::block::Architecture::Arm,
) | ((embassy_rp::block::IMAGE_TYPE_TBYB as u32) << 16)]);

/// this boot is a trial (src/ota.rs) not yet bought
static PENDING: AtomicBool = AtomicBool::new(false);

/// GP2 (the FP008's IRQ): its level, and whether it has ever been low (= an FP008 is on the line)
static IRQ_HIGH: AtomicBool = AtomicBool::new(true);
static SEEN_LOW: AtomicBool = AtomicBool::new(false);
/// when the host last wrote to the module, and when this firmware's own MID_VERIFY went out (ticks; 0 = none in
/// flight: its REPLY or AUTO_TIMEOUT_MS clears it)
static HOST_AT: AtomicU64 = AtomicU64::new(0);
static AUTO_AT: AtomicU64 = AtomicU64::new(0);
/// the module's timeout for a presence-started verify, and how long a host command keeps those off
const AUTO_VERIFY_S: u8 = 10;
const AUTO_TIMEOUT_MS: u64 = (AUTO_VERIFY_S as u64 + 3) * 1000;
const HOST_QUIET_MS: u64 = 20_000;

/// lines for the console from the other tasks (`EVT ...`); dropped when full
static EVT: Channel<CriticalSectionRawMutex, proto::Line, 4> = Channel::new();

type UartTx = Mutex<CriticalSectionRawMutex, BufferedUartTx>;

fn ms_since(ticks: u64) -> u64 {
    Duration::from_ticks(Instant::now().as_ticks().saturating_sub(ticks)).as_millis()
}

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbIrq<USB>;
    UART0_IRQ => BufferedInterruptHandler<UART0>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
    TRNG_IRQ => embassy_rp::trng::InterruptHandler<TRNG>;
});

type UsbDrv = Driver<'static, USB>;

/// the infrared sensor's latest heartbeat: bit 63 = valid, bits 0..40 = its 5 bytes (big-endian); and when, in ticks
static IR_HB: AtomicU64 = AtomicU64::new(0);
static IR_AT: AtomicU64 = AtomicU64::new(0);

/// UART1 RX (9600): the heartbeat has no header, so a packet is the bytes between two silences (one is >= 128 ms
/// apart, a byte is ~1 ms); anything that isn't 5 bytes long is dropped
#[embassy_executor::task]
async fn ir_task(mut rx: BufferedUartRx) -> ! {
    let mut pkt = [0u8; proto::IR_LEN];
    let mut n = 0usize;
    let mut buf = [0u8; 16];
    loop {
        match with_timeout(Duration::from_millis(20), rx.read(&mut buf)).await {
            Ok(Ok(k)) => {
                for &b in &buf[..k] {
                    if n < pkt.len() {
                        pkt[n] = b;
                    }
                    n = n.saturating_add(1);
                }
            }
            Ok(Err(_)) => n = pkt.len() + 1, // a framing / break error: drop this packet
            Err(_) => {
                if n == proto::IR_LEN {
                    let v = (1u64 << 63) | pkt.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
                    IR_AT.store(Instant::now().as_ticks(), Ordering::Relaxed);
                    IR_HB.store(v, Ordering::Relaxed);
                }
                n = 0;
            }
        }
    }
}

fn ir_latest() -> Option<([u8; proto::IR_LEN], u64)> {
    let v = IR_HB.load(Ordering::Relaxed);
    if v >> 63 == 0 {
        return None;
    }
    let b = v.to_be_bytes();
    let age = Instant::now().as_ticks().saturating_sub(IR_AT.load(Ordering::Relaxed));
    Some(([b[3], b[4], b[5], b[6], b[7]], Duration::from_ticks(age).as_millis()))
}

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, UsbDrv>) -> ! {
    usb.run().await
}

/// host (interface 1) -> UART0, byte for byte
#[embassy_executor::task]
async fn host_to_module(mut usb_rx: Receiver<'static, UsbDrv>, tx: &'static UartTx) -> ! {
    let mut buf = [0u8; 64];
    loop {
        usb_rx.wait_connection().await;
        while let Ok(n) = usb_rx.read_packet(&mut buf).await {
            HOST_AT.store(Instant::now().as_ticks(), Ordering::Relaxed);
            if tx.lock().await.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    }
}

/// UART0 -> host (interface 1) byte for byte, always read (a presence-started verify's REPLY is picked out here
/// even with no host); a host that doesn't read loses the bytes after 50 ms instead of stalling this
#[embassy_executor::task]
async fn module_to_host(mut usb_tx: Sender<'static, UsbDrv>, mut uart_rx: BufferedUartRx) -> ! {
    let mut buf = [0u8; 64];
    let mut fp = proto::FrameParser::new();
    loop {
        let Ok(n) = uart_rx.read(&mut buf).await else { continue };
        for &b in &buf[..n] {
            if let Some((mid, len)) = fp.push(b) {
                if AUTO_AT.load(Ordering::Relaxed) != 0 {
                    if let Some(l) = proto::verify_evt(mid, &fp.buf[..len]) {
                        AUTO_AT.store(0, Ordering::Relaxed);
                        let _ = EVT.try_send(l);
                    }
                }
            }
        }
        if n > 0 {
            let _ = with_timeout(Duration::from_millis(50), usb_tx.write_packet(&buf[..n])).await;
        }
    }
}

/// GP2: `EVT PRESENCE 1|0` on each change, and on an FP008's rising edge a MID_VERIFY of this firmware's own
#[embassy_executor::task]
async fn presence_task(mut irq: Input<'static>, tx: &'static UartTx) -> ! {
    loop {
        let high = irq.is_high();
        IRQ_HIGH.store(high, Ordering::Relaxed);
        if !high {
            SEEN_LOW.store(true, Ordering::Relaxed);
        }
        let at = AUTO_AT.load(Ordering::Relaxed);
        if at != 0 && ms_since(at) > AUTO_TIMEOUT_MS {
            AUTO_AT.store(0, Ordering::Relaxed); // no REPLY: give up on it
        }
        if with_timeout(Duration::from_secs(1), irq.wait_for_any_edge()).await.is_err() {
            continue;
        }
        Timer::after_millis(20).await; // settle
        let now_high = irq.is_high();
        if now_high == high {
            continue;
        }
        IRQ_HIGH.store(now_high, Ordering::Relaxed);
        if !now_high {
            SEEN_LOW.store(true, Ordering::Relaxed);
        }
        let _ = EVT.try_send(proto::fmt(format_args!("EVT PRESENCE {}", u8::from(now_high))));
        let host = HOST_AT.load(Ordering::Relaxed);
        let host_quiet = host == 0 || ms_since(host) > HOST_QUIET_MS;
        if now_high && SEEN_LOW.load(Ordering::Relaxed) && host_quiet && AUTO_AT.load(Ordering::Relaxed) == 0 {
            AUTO_AT.store(Instant::now().as_ticks().max(1), Ordering::Relaxed);
            if tx.lock().await.write_all(&proto::verify_frame(AUTO_VERIFY_S)).await.is_err() {
                AUTO_AT.store(0, Ordering::Relaxed);
            } else {
                let _ = EVT.try_send(proto::fmt(format_args!("EVT VERIFY START TIMEOUT_S={AUTO_VERIFY_S}")));
            }
        }
    }
}

/// Feeds the watchdog (the ROM arms it for a trial boot, src/ota.rs); stops feeding a trial left unbought for
/// ota::CONFIRM_TIMEOUT_MS, so the ROM boots the previous image again.
#[embassy_executor::task]
async fn watchdog_task(mut wd: Watchdog) -> ! {
    wd.start(Duration::from_secs(8));
    let start = Instant::now();
    loop {
        if PENDING.load(Ordering::Relaxed) && start.elapsed() > Duration::from_millis(ota::CONFIRM_TIMEOUT_MS) {
            wd.trigger_reset();
        }
        wd.feed(Duration::from_secs(8));
        Timer::after_secs(1).await;
    }
}

struct Console {
    class: CdcAcmClass<'static, UsbDrv>,
    flash: key::KeyFlash,
    trng: Trng<'static, TRNG>,
    seed: Option<[u8; 32]>,
    boot: ota::BootInfo,
    told: bool,
}

type UsbErr = embassy_usb::driver::EndpointError;

impl Console {
    async fn reply(&mut self, l: &str) -> Result<(), UsbErr> {
        write_line(&mut self.class, l).await
    }

    /// once per boot, on the first connection: the trial state (console-protocol.md §6 8.)
    async fn tell_boot(&mut self) -> Result<(), UsbErr> {
        if self.told {
            return Ok(());
        }
        self.told = true;
        // the host's tty may still be cooked (Linux echoes until it goes raw) and pyserial flushes input on open:
        // a line sent at the very connection is echoed back as a command and lost to the reader
        Timer::after_millis(200).await;
        let slot = ota::label(self.boot.partition);
        if PENDING.load(Ordering::Relaxed) {
            let l = proto::fmt(format_args!("EVT OTA_SERIAL_PENDING slot={slot} timeout_ms={}", ota::CONFIRM_TIMEOUT_MS));
            self.reply(&l).await?;
        }
        if self.boot.rolled_back {
            let l = proto::fmt(format_args!("EVT OTA_ROLLED_BACK slot={slot} reason=serial_unconfirmed"));
            self.reply(&l).await?;
        }
        Ok(())
    }

    async fn command(&mut self, line: &str) -> Result<(), UsbErr> {
        let reply = match proto::parse(line) {
            proto::Cmd::Ping => proto::line("PONG"),
            proto::Cmd::Device => proto::device_line(VERSION, SEEN_LOW.load(Ordering::Relaxed)),
            proto::Cmd::Keygen { force } => {
                if self.seed.is_some() && !force {
                    proto::line("ERR AUTH: key exists")
                } else {
                    let mut s = [0u8; 32];
                    self.trng.fill_bytes(&mut s).await;
                    if key::store(&mut self.flash, &s) {
                        self.seed = Some(s);
                        proto::pubkey_line(&s)
                    } else {
                        proto::line("ERR AUTH: flash")
                    }
                }
            }
            proto::Cmd::Pubkey => match &self.seed {
                Some(s) => proto::pubkey_line(s),
                None => proto::line("ERR AUTH: no key"),
            },
            proto::Cmd::Sign(nonce) => match &self.seed {
                None => proto::line("ERR AUTH: no key"),
                Some(s) => proto::sig_line(s, nonce).unwrap_or_else(|| proto::line("ERR AUTH: bad nonce")),
            },
            proto::Cmd::OtaConfirm => {
                if PENDING.load(Ordering::Relaxed) {
                    let mut work = [0u8; ota::CHUNK];
                    if ota::buy(&mut work) {
                        PENDING.store(false, Ordering::Relaxed);
                    }
                }
                proto::fmt(format_args!("OTA CONFIRMED RX={}", ota::CHUNK))
            }
            proto::Cmd::OtaSerial(size, flavor) => return self.ota_serial(size, flavor).await,
            proto::Cmd::Ir => proto::ir_line(ir_latest(), IRQ_HIGH.load(Ordering::Relaxed)),
            proto::Cmd::Unknown => proto::line("ERR UNSUPPORTED (fp001s)"),
        };
        self.reply(&reply).await
    }

    /// §6 1.-5.: the image in CHUNK pieces, each erased + written to the other partition, `OTA ACK <total>` after
    /// each; then a reboot into it on trial
    async fn ota_serial(&mut self, size: Option<u32>, flavor: &str) -> Result<(), UsbErr> {
        if flavor != proto::FLAVOR {
            return self.reply("OTA ERR flavor").await;
        }
        let Some(target) = ota::target(&self.boot) else { return self.reply("OTA ERR begin").await };
        let size = match size {
            Some(n) if (ota::MIN_IMAGE..=ota::PART_SIZE).contains(&n) => n,
            _ => return self.reply("OTA ERR size").await,
        };
        let l = proto::fmt(format_args!("OTA READY {} RX={}", ota::CHUNK, ota::CHUNK));
        self.reply(&l).await?;
        let mut chunk = [0u8; ota::CHUNK];
        let mut done: u32 = 0;
        let mut pkt = [0u8; 64];
        while done < size {
            let want = core::cmp::min(ota::CHUNK as u32, size - done) as usize;
            let mut have = 0;
            while have < want {
                match with_timeout(Duration::from_secs(10), self.class.read_packet(&mut pkt)).await {
                    Err(_) => return self.reply("OTA ERR timeout").await,
                    Ok(r) => {
                        let n = r?;
                        let take = core::cmp::min(n, want - have); // stop-and-wait: nothing past the chunk
                        chunk[have..have + take].copy_from_slice(&pkt[..take]);
                        have += take;
                    }
                }
            }
            if done == 0 && !ota::looks_like_image(&chunk[..want]) {
                return self.reply("OTA ERR verify").await;
            }
            chunk[want..].fill(0xFF);
            let at = target + done;
            let ok = self.flash.blocking_erase(at, at + ota::CHUNK as u32).is_ok()
                && self.flash.blocking_write(at, &chunk).is_ok();
            if !ok {
                return self.reply("OTA ERR write").await;
            }
            done += want as u32;
            let l = proto::fmt(format_args!("OTA ACK {done}"));
            self.reply(&l).await?;
        }
        self.reply("OTA OK").await?;
        Timer::after_millis(100).await; // let the reply out
        ota::reboot_into(self.boot.partition, target, 500)
    }
}

#[embassy_executor::task]
async fn console_task(mut con: Console) -> ! {
    let mut line: heapless::String<128> = heapless::String::new();
    loop {
        con.class.wait_connection().await;
        if con.tell_boot().await.is_err() {
            continue;
        }
        while EVT.try_receive().is_ok() {} // events from before this connection are stale
        let mut buf = [0u8; 64];
        'conn: loop {
            let n = match select(con.class.read_packet(&mut buf), EVT.receive()).await {
                Either::First(Ok(n)) => n,
                Either::First(Err(_)) => break 'conn,
                Either::Second(l) => {
                    if con.reply(&l).await.is_err() {
                        break 'conn;
                    }
                    continue;
                }
            };
            for &b in &buf[..n] {
                if b == b'\r' || b == b'\n' {
                    if line.is_empty() {
                        continue;
                    }
                    let l = line.clone();
                    line.clear();
                    // `OTA SERIAL` reads the image's bytes itself; the rest of this packet (none: the host waits for
                    // `OTA READY`) would be dropped
                    if con.command(l.as_str()).await.is_err() {
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
    static UART_TX: StaticCell<UartTx> = StaticCell::new();
    let uart_tx: &'static UartTx = UART_TX.init(Mutex::new(uart_tx));
    let (usb_tx, usb_rx) = bridge.split();

    static IR_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    let mut ic = UartConfig::default();
    ic.baudrate = 9_600;
    let ir_rx = BufferedUartRx::new(p.UART1, Irqs, p.PIN_5, IR_BUF.init([0; 64]), ic);
    // pull-up (not down: the RP2350's E9 latches a pulled-down pin high): the FP008 drives it both ways, and with no
    // line (the FP001S) it stays high, which is how the two are told apart
    let irq = Input::new(p.PIN_2, Pull::Up);

    let flash = Flash::new_blocking(p.FLASH);
    let trng = Trng::new(p.TRNG, Irqs, embassy_rp::trng::Config::default());

    spawner.spawn(usb_task(usb).unwrap());
    spawner.spawn(host_to_module(usb_rx, uart_tx).unwrap());
    spawner.spawn(module_to_host(usb_tx, uart_rx).unwrap());
    spawner.spawn(presence_task(irq, uart_tx).unwrap());
    spawner.spawn(ir_task(ir_rx).unwrap());
    let boot = ota::boot_info();
    PENDING.store(boot.buy_pending, Ordering::Relaxed);
    let mut flash = flash;
    let seed = key::load(&mut flash);
    spawner.spawn(watchdog_task(Watchdog::new(p.WATCHDOG)).unwrap());
    spawner.spawn(console_task(Console { class: console, flash, trng, seed, boot, told: false }).unwrap());
}
