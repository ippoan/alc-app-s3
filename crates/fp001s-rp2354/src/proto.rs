//! The console's pure part: line parsing and the reply lines, no hardware (alloc-free).
//!
//! The words and the reply formats are docs/console-protocol.md's (the 正本), the same as the other kinds:
//! `PING` -> `PONG`, `DEVICE` -> `DEVICE fp001s VER=<v> BOARD=<fp008|fp001s> FLAVOR=fp001s`, `AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` /
//! `AUTH SIGN <nonce>` as alc-hub-core's `alarm_key`, `OTA CONFIRM` / `OTA SERIAL <size> <flavor>` as §6 (ed25519, base64url without padding, the nonce is 32 lowercase
//! hex characters and is signed as is). Anything else: `ERR UNSUPPORTED (fp001s)`.
//!
//! ★ `alarm_key` is std (alc-hub-core); this is a no_std copy of the few functions this kind needs. Keep the two
//! the same, character for character (auth-worker verifies both). Merging them into one no_std crate is a follow-up.

use core::fmt::Write;
use ed25519_dalek::{Signer, SigningKey};
use heapless::String;

pub const KIND: &str = "fp001s";
pub const FLAVOR: &str = "fp001s";

/// A reply line (the longest is `AUTH SIG <43> <86>` = 140 characters).
pub type Line = String<192>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd<'a> {
    Ping,
    Device,
    Keygen { force: bool },
    Pubkey,
    Sign(&'a str),
    /// `OTA CONFIRM` (§6 0. / 7.)
    OtaConfirm,
    /// `OTA SERIAL <size> <flavor>` (§6 1.); None for a size that is not a number
    OtaSerial(Option<u32>, &'a str),
    /// `IR`: the module's infrared proximity sensor (FP008 manual ch. 7), the latest heartbeat
    Ir,
    Unknown,
}

pub fn parse(line: &str) -> Cmd<'_> {
    let mut w = line.split_ascii_whitespace();
    match (w.next(), w.next(), w.next(), w.next()) {
        (Some("PING"), None, ..) => Cmd::Ping,
        (Some("DEVICE"), None, ..) => Cmd::Device,
        (Some("AUTH"), Some("KEYGEN"), None, _) => Cmd::Keygen { force: false },
        (Some("AUTH"), Some("KEYGEN"), Some("FORCE"), None) => Cmd::Keygen { force: true },
        (Some("AUTH"), Some("PUBKEY"), None, _) => Cmd::Pubkey,
        (Some("AUTH"), Some("SIGN"), Some(n), None) => Cmd::Sign(n),
        (Some("OTA"), Some("CONFIRM"), None, _) => Cmd::OtaConfirm,
        (Some("IR"), None, ..) => Cmd::Ir,
        (Some("OTA"), Some("SERIAL"), Some(size), Some(flavor)) if w.next().is_none() => Cmd::OtaSerial(size.parse().ok(), flavor),
        _ => Cmd::Unknown,
    }
}

/// alarm_key::check_nonce: 32 lowercase hex characters.
pub fn nonce_ok(nonce: &str) -> bool {
    nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding (alarm_key's b64url_encode).
fn b64url(out: &mut Line, data: &[u8]) {
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        let k = c.len() + 1;
        for i in 0..k {
            let _ = out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
}

/// `DEVICE fp001s VER=<v> BOARD=<fp008|fp001s> FLAVOR=fp001s`: BOARD is the module, told by the presence line
/// (`fp008` once the IRQ pin has been seen low; the FP001S has no such line and the pin's pull-up keeps it high)
pub fn device_line(ver: &str, fp008: bool) -> Line {
    let board = if fp008 { "fp008" } else { "fp001s" };
    let mut l = Line::new();
    let _ = write!(l, "DEVICE {KIND} VER={ver} BOARD={board} FLAVOR={FLAVOR}");
    l
}

/// The module's frame: `EF AA <mid> <len:2 BE> <data> <xor of mid..data>`
pub const MID_REPLY: u8 = 0x00;
pub const MID_VERIFY: u8 = 0x12;

/// MID_VERIFY [pd_rightaway = 0, timeout s], ready to write to the module's UART
pub fn verify_frame(timeout_s: u8) -> [u8; 8] {
    let body = [MID_VERIFY, 0x00, 0x02, 0x00, timeout_s];
    let p = body.iter().fold(0u8, |a, &b| a ^ b);
    [0xEF, 0xAA, body[0], body[1], body[2], body[3], body[4], p]
}

/// Picks the module's small frames out of its UART stream (images and templates, longer than CAP, are skipped).
pub struct FrameParser {
    st: u8,
    hdr: [u8; 3],
    hn: usize,
    len: usize,
    n: usize,
    pub buf: [u8; FrameParser::CAP],
}

impl FrameParser {
    pub const CAP: usize = 64;
    pub const fn new() -> Self {
        Self { st: 0, hdr: [0; 3], hn: 0, len: 0, n: 0, buf: [0; Self::CAP] }
    }

    /// Feeds one byte; Some((mid, len)) when a whole frame of at most CAP data bytes has ended (its data in
    /// buf[..len]; the parity isn't checked)
    pub fn push(&mut self, b: u8) -> Option<(u8, usize)> {
        match self.st {
            0 => self.st = u8::from(b == 0xEF),
            1 => self.st = if b == 0xAA { self.hn = 0; 2 } else { u8::from(b == 0xEF) },
            2 => {
                self.hdr[self.hn] = b;
                self.hn += 1;
                if self.hn == 3 {
                    self.len = usize::from(u16::from_be_bytes([self.hdr[1], self.hdr[2]]));
                    self.n = 0;
                    self.st = 3;
                }
            }
            _ => {
                if self.n < self.len {
                    if self.n < Self::CAP {
                        self.buf[self.n] = b;
                    }
                    self.n += 1;
                } else {
                    self.st = 0; // the parity byte
                    if self.len <= Self::CAP {
                        return Some((self.hdr[0], self.len));
                    }
                }
            }
        }
        None
    }
}

/// The REPLY to MID_VERIFY (data = [0x12, result, id:2, name:32, ...]) as `EVT VERIFY OK ID=<id> NAME=<name>` or
/// `EVT VERIFY NG RESULT=<n>`; None for any other frame
pub fn verify_evt(mid: u8, data: &[u8]) -> Option<Line> {
    if mid != MID_REPLY || data.first() != Some(&MID_VERIFY) || data.len() < 2 {
        return None;
    }
    if data[1] != 0 || data.len() < 4 {
        return Some(fmt(format_args!("EVT VERIFY NG RESULT={}", data[1])));
    }
    let id = u16::from_be_bytes([data[2], data[3]]);
    let name = data.get(4..36).unwrap_or(&[]);
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let name = core::str::from_utf8(&name[..end]).unwrap_or("?").trim();
    Some(fmt(format_args!("EVT VERIFY OK ID={id} NAME={name}")))
}

pub fn pubkey_line(seed: &[u8; 32]) -> Line {
    let mut l = Line::new();
    let _ = l.push_str("AUTH PUBKEY ");
    b64url(&mut l, SigningKey::from_bytes(seed).verifying_key().as_bytes());
    l
}

/// `AUTH SIG <pubkey> <sig>`: the signature over the nonce's ASCII as is (alarm_key::sign_nonce). None if the
/// nonce is malformed (the caller answers `ERR AUTH: bad nonce`).
pub fn sig_line(seed: &[u8; 32], nonce: &str) -> Option<Line> {
    if !nonce_ok(nonce) {
        return None;
    }
    let key = SigningKey::from_bytes(seed);
    let mut l = Line::new();
    let _ = l.push_str("AUTH SIG ");
    b64url(&mut l, key.verifying_key().as_bytes());
    let _ = l.push(' ');
    b64url(&mut l, &key.sign(nonce.as_bytes()).to_bytes());
    Some(l)
}

/// One heartbeat of the infrared sensor's own serial port (FP008 manual ch. 7, 9600 8N1): infrared value (2) +
/// trigger threshold (2) + emission current setting (1), 5 bytes with no header; one every ~0.5 s (0.128 s while
/// something is sensed). The manual doesn't say the byte order: big-endian is taken, and RAW= carries the bytes.
pub const IR_LEN: usize = 5;

/// `IR VALUE=<n> THRESHOLD=<n> CURRENT=<n> AGE_MS=<ms> IRQ=<0|1> RAW=<10 hex>`, or `IR NONE IRQ=<0|1>` before the
/// first heartbeat. IRQ is the module's pin 5 (High while the sensor is on: value >= threshold).
pub fn ir_line(hb: Option<([u8; IR_LEN], u64)>, irq: bool) -> Line {
    let irq = u8::from(irq);
    let Some((b, age_ms)) = hb else { return fmt(format_args!("IR NONE IRQ={irq}")) };
    let value = u16::from_be_bytes([b[0], b[1]]);
    let threshold = u16::from_be_bytes([b[2], b[3]]);
    fmt(format_args!(
        "IR VALUE={value} THRESHOLD={threshold} CURRENT={} AGE_MS={age_ms} IRQ={irq} RAW={:02X}{:02X}{:02X}{:02X}{:02X}",
        b[4], b[0], b[1], b[2], b[3], b[4]
    ))
}

pub fn fmt(args: core::fmt::Arguments<'_>) -> Line {
    let mut l = Line::new();
    let _ = l.write_fmt(args);
    l
}

pub fn line(s: &str) -> Line {
    let mut l = Line::new();
    let _ = l.push_str(s);
    l
}
