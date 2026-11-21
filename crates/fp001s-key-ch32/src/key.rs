//! The device key: an ed25519 seed (32 B) the board makes itself (src/noise.rs) and keeps in the flash's last
//! 256 B page (memory.x keeps the code out of it), as fp001s-rp2354 keeps it in its last sector. No read-out
//! protection and no option bytes are touched (nothing here is irreversible, the user's choice 2026-11-18). A
//! reflash with a full-chip erase loses the key; `AUTH KEYGEN` makes a new one and the host registers its
//! `AUTH PUBKEY` again.
//!
//! Page layout: MAGIC (4) | seed (32) | the rest 0xFF (alarm_key_nostd::record). Anything else reads as "no key".

use ch32_hal::flash::{Blocking, Flash};

/// the official 64 KiB (CH32V203F8U6; memory.x leaves the last page out)
const FLASH_SIZE: u32 = 64 * 1024;
/// the fast-erase / page-program unit
const PAGE: u32 = 256;
pub const KEY_OFFSET: u32 = FLASH_SIZE - PAGE;

pub type KeyFlash = Flash<'static, Blocking>;

pub fn load(flash: &mut KeyFlash) -> Option<[u8; 32]> {
    let mut buf = [0u8; alarm_key_nostd::RECORD_LEN];
    flash.blocking_read(KEY_OFFSET, &mut buf).ok()?;
    alarm_key_nostd::parse_record(&buf)
}

pub fn store(flash: &mut KeyFlash, seed: &[u8; 32]) -> bool {
    let mut buf = [0xFFu8; PAGE as usize];
    buf[..alarm_key_nostd::RECORD_LEN].copy_from_slice(&alarm_key_nostd::record(seed));
    flash.blocking_erase(KEY_OFFSET, KEY_OFFSET + PAGE).is_ok()
        && flash.blocking_write(KEY_OFFSET, &buf).is_ok()
        && load(flash).as_ref() == Some(seed)
}
