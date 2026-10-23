MEMORY {
    /* NOTE 1 K = 1 KiBi = 1024 bytes */
    /* Pico 2: 4 MiB flash. The boot ROM finds the IMAGE_DEF block
       (from embassy-rp, `.start_block`) in the first 4 KiB of flash */
    FLASH : ORIGIN = 0x10000000, LENGTH = 4096K
    /* SRAM0..7, striped */
    RAM   : ORIGIN = 0x20000000, LENGTH = 512K
    /* SRAM8/9 (SRAM4/5 in the datasheet's older naming), not striped; unused */
    SRAM4 : ORIGIN = 0x20080000, LENGTH = 4K
    SRAM5 : ORIGIN = 0x20081000, LENGTH = 4K
}

SECTIONS {
    /* ### Boot ROM info
     *
     * Goes after .vector_table, to keep it in the first 4K of flash
     * where the Boot ROM can find it
     */
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH

} INSERT AFTER .vector_table;

/* move .text to start /after/ the boot info */
_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    /* ### Boot ROM extra info
     *
     * Goes after everything in our program, so it can contain a signature.
     */
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH

} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);
