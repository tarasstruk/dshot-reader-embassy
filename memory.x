MEMORY {
    /* NOTE 1 K = 1 KiBi = 1024 bytes */
    /* Second stage bootloader, provided by embassy-rp (boot2-w25q080 by default) */
    BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
    /* Adjust this to suit the size of your specific flash chip */
    FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100
    RAM   : ORIGIN = 0x20000000, LENGTH = 264K
}
