//! The console's pure part: line parsing and the reply lines, no hardware (alloc-free).
//!
//! The words and the reply formats are docs/console-protocol.md's (the 正本), the same as the other kinds:
//! `PING` -> `PONG`, `DEVICE` -> `DEVICE fp001s VER=<v> FLAVOR=fp001s`, `AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` /
//! `AUTH SIGN <nonce>` as alc-hub-core's `alarm_key` (ed25519, base64url without padding, the nonce is 32 lowercase
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

pub fn line(s: &str) -> Line {
    let mut l = Line::new();
    let _ = l.push_str(s);
    l
}
