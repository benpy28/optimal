/* Optimal Boot, BIOS stage: one ELF holding stage 1 (the MBR code, at
 * 0x7C00) and stage 2 (at 0x8000). The installer copies the bytes of each
 * PT_LOAD segment to disk; nothing loads this ELF as an ELF. */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(stage1_start)
PHDRS
{
    stage1 PT_LOAD FLAGS(7);
    stage2 PT_LOAD FLAGS(7);
    lowbss PT_LOAD FLAGS(6);
}
SECTIONS
{
    .lowbss 0x500 (NOLOAD) : {
        __lowbss_start = .;
        *(.lowbss)
        __lowbss_end = .;
    } :lowbss
    ASSERT(__lowbss_end <= 0x1000, "low variables overlap the real-mode stack")

    .stage1 0x7c00 : { KEEP(*(.stage1)) } :stage1
    ASSERT(SIZEOF(.stage1) == 440, "stage 1 must be exactly the 440 bytes of MBR code")

    . = 0x8000;
    __stage2_start = .;
    .stage2_head : {
        KEEP(*(.stage2_head))
        __text16_end = .;
    } :stage2
    ASSERT(__text16_end <= 0x10000, "real-mode code must stay below 64 KiB")
    .text : { *(.text .text.*) } :stage2
    .rodata : { *(.rodata .rodata.*) } :stage2
    .data : { *(.data .data.*) *(.got .got.*) } :stage2
    __stage2_file_end = .;
    .bss (NOLOAD) : {
        . = ALIGN(4096);
        __bss_start = .;
        *(.bss.boot_tables)
        *(.bss .bss.*) *(COMMON)
        . = ALIGN(16);
        __stack_bottom = .;
        . += 0x20000;
        __stack_top = .;
        __bss_end = .;
    } :stage2
    ASSERT(__bss_end <= 0x70000, "stage 2 is too large: it would overlap the disk bounce buffer")
    /DISCARD/ : { *(.eh_frame*) *(.note .note.*) *(.comment*) }
}
