/* Optimal kernel layout. The kernel is a static position-independent
   executable: it is linked at the start of the top 2 GiB, and Limine slides
   it to a random address (KASLR) and applies the relocations listed in
   .rela.dyn, which it finds through the PT_DYNAMIC segment. */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kmain)

PHDRS
{
    /* Explicit permissions so Limine maps the kernel W^X:
       code is read+execute, rodata read-only, data read+write. */
    text    PT_LOAD FLAGS(5);
    rodata  PT_LOAD FLAGS(4);
    relro   PT_LOAD FLAGS(6);
    data    PT_LOAD FLAGS(6);
    dynamic PT_DYNAMIC FLAGS(6);
}

SECTIONS
{
    . = 0xffffffff80000000;
    __kernel_start = .;

    .text : {
        *(.text .text.*)
    } :text

    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __text_end = .;
    __rodata_start = .;

    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    /* Relocation and symbol tables Limine reads while loading us. */
    .dynsym   : { *(.dynsym) } :rodata
    .dynstr   : { *(.dynstr) } :rodata
    .hash     : { *(.hash) } :rodata
    .gnu.hash : { *(.gnu.hash) } :rodata
    .rela.dyn : { *(.rela.dyn .rela.*) } :rodata

    .ktests : {
        __ktests_start = .;
        KEEP(*(.ktests))
        __ktests_end = .;
    } :rodata

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    /* Data that only relocation writes to (vtables, pointer tables, the GOT).
       Limine maps it writable to apply relocations; the kernel's own page
       tables map it read-only afterwards (RELRO). Page aligned at both ends
       so those page permissions cover exactly this. */
    __relro_start = .;
    .data.rel.ro : { *(.data.rel.ro .data.rel.ro.*) } :relro
    .dynamic : { *(.dynamic) } :relro :dynamic
    .got : { *(.got .got.*) } :relro
    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __relro_end = .;
    __data_start = .;

    .data : {
        /* Limine scans for its requests between these markers. */
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
        *(.data .data.*)
    } :data


    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
    }
}
