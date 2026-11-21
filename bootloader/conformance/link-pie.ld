/* Position-independent build, linked in the LOWER half on purpose: the
   loader must slide it into the top 2 GiB and apply every relocation. */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)
PHDRS
{
    text    PT_LOAD FLAGS(5);
    rodata  PT_LOAD FLAGS(4);
    data    PT_LOAD FLAGS(6);
    dynamic PT_DYNAMIC FLAGS(6);
}
SECTIONS
{
    . = 0x1000;
    __image_start = .;
    .text : { __text_start = .; *(.text .text.*) __text_end = .; } :text
    . = ALIGN(4096);
    .rodata : { __rodata_start = .; *(.rodata .rodata.*) __rodata_end = .; } :rodata
    .rela.dyn : { *(.rela.dyn .rela.*) } :rodata
    .relr.dyn : { *(.relr.dyn) } :rodata
    .dynsym : { *(.dynsym) } :rodata
    .dynstr : { *(.dynstr) } :rodata
    .hash : { *(.hash) } :rodata
    .gnu.hash : { *(.gnu.hash) } :rodata
    . = ALIGN(4096);
    .data : {
        __data_start = .;
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
        *(.data .data.*)
    } :data
    .dynamic : { *(.dynamic) } :data :dynamic
    .got : { *(.got .got.*) } :data
    .bss : { *(.bss .bss.*) *(COMMON) __data_end = .; } :data
    __image_end = .;
    /DISCARD/ : { *(.eh_frame*) *(.note .note.*) *(.interp) }
}
