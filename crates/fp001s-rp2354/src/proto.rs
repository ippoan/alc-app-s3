//! The console's pure part: line parsing and the reply lines, no hardware (alloc-free).
//!
//! The words and the reply formats are docs/console-protocol.md's (the 正本), the same as the other kinds:
//! `PING` -> `PONG`, `DEVICE` -> `DEVICE fp001s VER=<v> FLAVOR=fp001s`, `AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` /
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

pub fn device_line(ver: &str) -> Line {
    let mut l = Line::new();
    let _ = write!(l, "DEVICE {KIND} VER={ver} FLAVOR={FLAVOR}");
    l
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
