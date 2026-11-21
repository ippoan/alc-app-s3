//! `alc_hub_core::alarm_key` (std, ESP) の no_std 版: AUTH の応答行と、コンソールの共通の語の読み取り。
//!
//! ESP 以外の機種 (`crates/fp001s-rp2354` = t3 の RP2354A、`crates/fp001s-key-ch32` = t4 の鍵専用 CH32V203) が
//! 共有する。語と応答の形は docs/console-protocol.md (正本) のとおり:
//! `PING` -> `PONG`、`DEVICE` -> `DEVICE <kind> VER=<v> FLAVOR=<flavor>`、`AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` /
//! `AUTH SIGN <nonce>` (ed25519、base64url の padding 無し、nonce は小文字 hex 32 文字で ASCII のまま署名する)。
//!
//! ★ **`AUTH PUBKEY` / `AUTH SIG` の行は `alarm_key` と 1 文字も違えないこと** — サーバー (auth-worker) と
//! ブラウザが同じ行を読む。tests/same_as_hub_core.rs が同じ seed・nonce で突き合わせる。
//!
//! 鍵の記録 ([`record`] / [`parse_record`]) は MAGIC (4) | seed (32) で、置き場 (フラッシュのどこか) は機種が決める。
//! 秘密鍵 (seed) を外へ出す関数はここにも機種側にも作らない。

#![no_std]

#[cfg(feature = "entropy")]
pub mod entropy;

use core::fmt::Write;
use salty::Keypair;
use heapless::String;

/// A reply line (the longest is `AUTH SIG <43> <86>` = 140 characters).
pub type Line = String<192>;

/// alarm_key::ERR_BAD_NONCE
pub const ERR_BAD_NONCE: &str = "ERR AUTH: bad nonce";
pub const ERR_KEY_EXISTS: &str = "ERR AUTH: key exists";
pub const ERR_NO_KEY: &str = "ERR AUTH: no key";
pub const ERR_FLASH: &str = "ERR AUTH: flash";

/// The words every kind answers the same way. A kind with more words (OTA, ...) reads those first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd<'a> {
    Ping,
    Device,
    Keygen { force: bool },
    Pubkey,
    Sign(&'a str),
    /// anything else (the kind answers `ERR UNSUPPORTED (<kind>)` or reads its own words)
    Other,
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
        _ => Cmd::Other,
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

/// `DEVICE <kind> VER=<ver> FLAVOR=<flavor>` (console-protocol.md §2)
pub fn device_line(kind: &str, ver: &str, flavor: &str) -> Line {
    fmt(format_args!("DEVICE {kind} VER={ver} FLAVOR={flavor}"))
}

/// `ERR UNSUPPORTED (<kind>)`
pub fn unsupported_line(kind: &str) -> Line {
    fmt(format_args!("ERR UNSUPPORTED ({kind})"))
}

/// `AUTH PUBKEY <base64url>` (alarm_key::auth_pubkey_line)
pub fn pubkey_line(seed: &[u8; 32]) -> Line {
    let mut l = line("AUTH PUBKEY ");
    b64url(&mut l, Keypair::from(seed).public.compressed.as_bytes());
    l
}

/// `AUTH SIG <pubkey> <sig>`: the signature over the nonce's ASCII as is (alarm_key::auth_sig_line). None if the
/// nonce is malformed (the caller answers [`ERR_BAD_NONCE`]).
pub fn sig_line(seed: &[u8; 32], nonce: &str) -> Option<Line> {
    if !nonce_ok(nonce) {
        return None;
    }
    let key = Keypair::from(seed);
    let mut l = line("AUTH SIG ");
    b64url(&mut l, key.public.compressed.as_bytes());
    let _ = l.push(' ');
    // RFC 8032's deterministic signature: the same bytes as alarm_key's ed25519-dalek
    b64url(&mut l, &key.sign(nonce.as_bytes()).to_bytes());
    Some(l)
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

/// The key record's magic (the same on every kind; the place is the kind's: its flash's last sector / page).
pub const MAGIC: [u8; 4] = *b"FPK1";
pub const RECORD_LEN: usize = 4 + 32;

/// MAGIC (4) | seed (32)
pub fn record(seed: &[u8; 32]) -> [u8; RECORD_LEN] {
    let mut r = [0u8; RECORD_LEN];
    r[..4].copy_from_slice(&MAGIC);
    r[4..].copy_from_slice(seed);
    r
}

/// The seed, if `bytes` starts with a record. Anything else (an erased 0xFF page, ...) reads as "no key".
pub fn parse_record(bytes: &[u8]) -> Option<[u8; 32]> {
    if bytes.len() < RECORD_LEN || bytes[..4] != MAGIC {
        return None;
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes[4..RECORD_LEN]);
    Some(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_words() {
        assert_eq!(parse("PING"), Cmd::Ping);
        assert_eq!(parse("  DEVICE "), Cmd::Device);
        assert_eq!(parse("AUTH KEYGEN"), Cmd::Keygen { force: false });
        assert_eq!(parse("AUTH KEYGEN FORCE"), Cmd::Keygen { force: true });
        assert_eq!(parse("AUTH KEYGEN FORCE X"), Cmd::Other);
        assert_eq!(parse("AUTH PUBKEY"), Cmd::Pubkey);
        assert_eq!(parse("AUTH SIGN abc"), Cmd::Sign("abc"));
        assert_eq!(parse("AUTH SIGN a b"), Cmd::Other);
        assert_eq!(parse("PING x"), Cmd::Other);
        assert_eq!(parse("ping"), Cmd::Other);
        assert_eq!(parse(""), Cmd::Other);
    }

    #[test]
    fn lines() {
        assert_eq!(device_line("fp001s-key", "0.1.0", "fp001s-key").as_str(), "DEVICE fp001s-key VER=0.1.0 FLAVOR=fp001s-key");
        assert_eq!(unsupported_line("fp001s-key").as_str(), "ERR UNSUPPORTED (fp001s-key)");
    }

    #[test]
    fn record_round_trip() {
        let seed = [7u8; 32];
        let r = record(&seed);
        assert_eq!(parse_record(&r), Some(seed));
        let mut page = [0xFFu8; 64];
        assert_eq!(parse_record(&page), None);
        page[..RECORD_LEN].copy_from_slice(&r);
        assert_eq!(parse_record(&page), Some(seed));
        assert_eq!(parse_record(&r[..RECORD_LEN - 1]), None);
    }
}
