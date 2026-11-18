//! The device key: an ed25519 seed (32 B) the board makes itself (TRNG) and keeps in the flash's last 4 KiB
//! sector, as the VoiceS3R keeps its seed in NVS (`alarm_sk`). Not in OTP and no Secure Boot (the user's choice,
//! 2026-11-18): nothing here is irreversible. A reflash that wipes the sector loses the key; `AUTH KEYGEN` makes a
//! new one and the host registers its `AUTH PUBKEY` again.
//!
//! Sector layout: MAGIC (4) | seed (32) | the rest 0xFF. Anything else reads as "no key".

use embassy_rp::flash::{Blocking, Flash, ERASE_SIZE};
use embassy_rp::peripherals::FLASH;

/// RP2354A: 2 MiB inside; the Pico 2 (4 MiB) uses the same offset. The sector is outside the A / B partitions
/// (src/ota.rs, partition_table.json), so a serial OTA never touches it.
pub const FLASH_SIZE: usize = 2 * 1024 * 1024;
pub const KEY_OFFSET: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;
const MAGIC: [u8; 4] = *b"FPK1";

pub type KeyFlash = Flash<'static, FLASH, Blocking, FLASH_SIZE>;

pub fn load(flash: &mut KeyFlash) -> Option<[u8; 32]> {
    let mut buf = [0u8; 36];
    flash.blocking_read(KEY_OFFSET, &mut buf).ok()?;
    if buf[..4] != MAGIC {
        return None;
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&buf[4..]);
    Some(seed)
}

pub fn store(flash: &mut KeyFlash, seed: &[u8; 32]) -> bool {
    let mut buf = [0xFFu8; 256]; // one flash page
    buf[..4].copy_from_slice(&MAGIC);
    buf[4..36].copy_from_slice(seed);
    flash.blocking_erase(KEY_OFFSET, KEY_OFFSET + ERASE_SIZE as u32).is_ok()
        && flash.blocking_write(KEY_OFFSET, &buf).is_ok()
        && load(flash).as_ref() == Some(seed)
}
