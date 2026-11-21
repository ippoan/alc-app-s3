/* CH32V203F8U6: 64 KiB flash, 20 KiB SRAM (ch32-metapac's ch32v203f8u6, the official sizes).
   The flash's last 256 B page (0xFF00..0x10000) is the key's (src/key.rs): the code never links there. */
MEMORY
{
    FLASH (rx)  : ORIGIN = 0x00000000, LENGTH = 64K - 256
    RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 20K
}

REGION_ALIAS("REGION_TEXT", FLASH);
REGION_ALIAS("REGION_RODATA", FLASH);
REGION_ALIAS("REGION_DATA", RAM);
REGION_ALIAS("REGION_BSS", RAM);
REGION_ALIAS("REGION_HEAP", RAM);
REGION_ALIAS("REGION_STACK", RAM);
