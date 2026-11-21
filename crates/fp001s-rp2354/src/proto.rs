//! The console's pure part: line parsing and the reply lines, no hardware (alloc-free).
//!
//! The words and the reply formats are docs/console-protocol.md's (the 正本), the same as the other kinds:
//! `PING` -> `PONG`, `DEVICE` -> `DEVICE fp001s VER=<v> FLAVOR=fp001s`, `AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` /
//! `AUTH SIGN <nonce>` as alc-hub-core's `alarm_key` (crates/alarm-key-nostd, shared with fp001s-key-ch32),
//! `OTA CONFIRM` / `OTA SERIAL <size> <flavor>` as §6. Anything else: `ERR UNSUPPORTED (fp001s)`.

pub use alarm_key_nostd::{fmt, line, pubkey_line, sig_line, Line};

pub const KIND: &str = "fp001s";
pub const FLAVOR: &str = "fp001s";

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
    Unknown,
}

pub fn parse(line: &str) -> Cmd<'_> {
    let mut w = line.split_ascii_whitespace();
    match (w.next(), w.next(), w.next(), w.next()) {
        (Some("OTA"), Some("CONFIRM"), None, _) => return Cmd::OtaConfirm,
        (Some("OTA"), Some("SERIAL"), Some(size), Some(flavor)) if w.next().is_none() => {
            return Cmd::OtaSerial(size.parse().ok(), flavor)
        }
        _ => {}
    }
    match alarm_key_nostd::parse(line) {
        alarm_key_nostd::Cmd::Ping => Cmd::Ping,
        alarm_key_nostd::Cmd::Device => Cmd::Device,
        alarm_key_nostd::Cmd::Keygen { force } => Cmd::Keygen { force },
        alarm_key_nostd::Cmd::Pubkey => Cmd::Pubkey,
        alarm_key_nostd::Cmd::Sign(n) => Cmd::Sign(n),
        alarm_key_nostd::Cmd::Other => Cmd::Unknown,
    }
}

pub fn device_line(ver: &str) -> Line {
    alarm_key_nostd::device_line(KIND, ver, FLAVOR)
}
