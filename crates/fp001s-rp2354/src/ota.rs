//! Serial OTA (docs/console-protocol.md §6, the same lines as the ESP kinds) on the RP2350's own A/B scheme.
//!
//! Flash (partition_table.json, written once at the factory with picotool):
//!   0x000000  8 KiB   the partition table
//!   0x002000  1016 KiB  partition A (id 0)
//!   0x100000  1016 KiB  partition B (id 1, linked to A)
//!   0x1FE000  8 KiB   unpartitioned; the device key in its last 4 KiB (src/key.rs)
//! The image is linked at 0x10000000 and runs from either partition (the boot ROM's address translation); it is the
//! ELF's flat binary (`objcopy -O binary`), which the host sends as is.
//!
//! The image is "try before you buy" (TBYB, its IMAGE_DEF in main.rs): after `OTA OK` the board reboots into the
//! new partition on trial. `OTA CONFIRM` buys it (`explicit_buy`); without one in 10 minutes the app stops feeding
//! the watchdog, the ROM drops the unbought image and boots the other partition again, which then reports
//! `EVT OTA_ROLLED_BACK reason=serial_unconfirmed`. The ROM leaves no sign of a fallback (its OTHER_ERASED flag is
//! the version-downgrade erase on a buy, not this), so before the trial reboot we note the partition we leave in the
//! watchdog's scratch 0/1 (the ROM keeps 4-7 for itself; a watchdog reset keeps them, a power cut does not — a trial
//! cut off by unplugging falls back silently). An image that dies before it can feed the watchdog falls back
//! the same way (the ROM arms the watchdog for a TBYB boot), unlike the ESP kinds (console-protocol.md 既知の限界).

use embassy_rp::rom_data;

pub const XIP_BASE: u32 = 0x1000_0000;
pub const PART_A: u32 = 0x0000_2000;
pub const PART_B: u32 = 0x0010_0000;
pub const PART_SIZE: u32 = 1016 * 1024;
pub const CHUNK: usize = 4096;
/// smaller than this is not an image of ours (the image is ~70 KiB)
pub const MIN_IMAGE: u32 = 16 * 1024;
pub const CONFIRM_TIMEOUT_MS: u64 = 600_000;

const SYS_INFO_BOOT_INFO: u32 = 0x0040;
const TBYB_BUY_PENDING: u32 = 0x01;
/// watchdog scratch 0 while a trial is out: "TRIA"; scratch 1: the partition left for it
const TRIAL_MAGIC: u32 = 0x5452_4941;
const REBOOT_TYPE_FLASH_UPDATE: u32 = 0x0004;
const REBOOT_NO_RETURN: u32 = 0x0100;

#[derive(Clone, Copy)]
pub struct BootInfo {
    /// the partition this image runs from (0 = A, 1 = B), -1 when the flash has no partition table
    pub partition: i8,
    pub buy_pending: bool,
    /// the previous image (the other partition) was a trial that was not bought: we are the fallback
    pub rolled_back: bool,
}

pub fn boot_info() -> BootInfo {
    let mut buf = [0u32; 5];
    let n = unsafe { rom_data::get_sys_info(buf.as_mut_ptr(), buf.len(), SYS_INFO_BOOT_INFO) };
    if n < 2 || buf[0] & SYS_INFO_BOOT_INFO == 0 {
        return BootInfo { partition: -1, buy_pending: false, rolled_back: false };
    }
    let w = buf[1]; // diagnostic partition | boot type << 8 | partition << 16 | tbyb_and_update_info << 24
    let partition = ((w >> 16) & 0xFF) as u8 as i8;
    let buy_pending = (w >> 24) & TBYB_BUY_PENDING != 0;
    let wd = embassy_rp::pac::WATCHDOG;
    let rolled_back =
        !buy_pending && wd.scratch0().read() == TRIAL_MAGIC && wd.scratch1().read() == partition as u8 as u32;
    if !buy_pending {
        wd.scratch0().write_value(0); // the trial is over either way: bought (we are it) or fallen back (we are this)
    }
    BootInfo { partition, buy_pending, rolled_back }
}

/// The partition an update goes to: the one we are not running from. None without a partition table (an image
/// loaded flat at 0x0 for development cannot be updated over the serial line).
pub fn target(info: &BootInfo) -> Option<u32> {
    match info.partition {
        0 => Some(PART_B),
        1 => Some(PART_A),
        _ => None,
    }
}

/// The first 4 KiB of an image of ours hold its IMAGE_DEF block (the ROM's block loop): its start marker after the
/// vector table. A cheap check that the host sent an RP2350 image, not something else.
pub fn looks_like_image(first: &[u8]) -> bool {
    first.chunks_exact(4).any(|w| w == 0xffff_ded3u32.to_le_bytes())
}

pub fn label(partition: i8) -> &'static str {
    match partition {
        0 => "A",
        1 => "B",
        _ => "-",
    }
}

/// Buy the running trial image (the ROM rewrites its first sector; `work` is its 4 KiB work area).
pub fn buy(work: &mut [u8; CHUNK]) -> bool {
    let ok = unsafe { rom_data::explicit_buy(work.as_mut_ptr(), CHUNK as u32) == 0 };
    if ok {
        embassy_rp::pac::WATCHDOG.scratch0().write_value(0);
    }
    ok
}

/// Reboot into the freshly written partition (on trial), after `delay_ms`; `from` is the partition we run from.
pub fn reboot_into(from: i8, partition_start: u32, delay_ms: u32) -> ! {
    let wd = embassy_rp::pac::WATCHDOG;
    wd.scratch1().write_value(from as u8 as u32);
    wd.scratch0().write_value(TRIAL_MAGIC);
    rom_data::reboot(REBOOT_TYPE_FLASH_UPDATE | REBOOT_NO_RETURN, delay_ms, XIP_BASE + partition_start, 0);
    loop {
        cortex_m::asm::wfi();
    }
}
