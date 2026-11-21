OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)
PHDRS
{
    text    PT_LOAD FLAGS(5);
    rodata  PT_LOAD FLAGS(4);
    data    PT_LOAD FLAGS(6);
}
SECTIONS
{
    . = 0xffffffff80000000;
    __image_start = .;
    .text : { __text_start = .; *(.text .text.*) __text_end = .; } :text
    . = ALIGN(4096);
    .rodata : { __rodata_start = .; *(.rodata .rodata.*) __rodata_end = .; } :rodata
    . = ALIGN(4096);
    .data : {
        __data_start = .;
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
        *(.data .data.*)
    } :data
    .bss : { *(.bss .bss.*) *(COMMON) __data_end = .; } :data
    __image_end = .;
    /DISCARD/ : { *(.eh_frame*) *(.note .note.*) }
}
