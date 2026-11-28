# Optimal Boot, BIOS stage 2: the header, the 16-bit entry, the switch to
# long mode, and the thunk that lets 64-bit Rust code call BIOS interrupts.
#
# Memory below 1 MiB while stage 2 runs (DESIGN.md, BIOS stage):
#   0x00000-0x004FF  real-mode IVT and BIOS data area (never written)
#   0x00500-0x00FFF  .lowbss: variables shared with real mode
#   0x01000-0x07BFF  real-mode stack (SS:SP = 0000:7C00)
#   0x07C00-0x07DFF  stage 1
#   0x08000-0x6FFFF  stage 2: this file, then Rust code, data and .bss
#   0x70000-0x7FFFF  disk bounce buffer
# The 16-bit code and everything it touches sit below 0x10000, so real
# mode runs with every segment register 0.

.section .stage2_head, "awx"
.code16
.global stage2_header
stage2_header:
    .ascii  "OBSTAGE2"
    # The installer fills these in (see install/src/bios.rs):
    .long   0               # 0x08 image size in bytes
    .long   0               # 0x0C CRC-32 of the image from 0x40 to the end
    .byte   0               # 0x10 volume kind: 1 = GPT, 2 = MBR
    .byte   0               # 0x11 MBR partition number (1-4)
    .word   0               # 0x12 reserved
    .long   0               # 0x14 reserved
    .quad   0               # 0x18 boot volume's first LBA
    .quad   0, 0            # 0x20 GPT unique partition GUID
    .long   1               # 0x30 header version
    .long   0, 0, 0         # 0x34 reserved
    # 0x40: entry from stage 1, DL = BIOS drive number.
stage2_entry:
    cli
    xor     %ax, %ax
    mov     %ax, %ds
    mov     %ax, %es
    mov     %ax, %ss
    mov     $0x7c00, %sp
    cld
    # Zero .lowbss first, then keep the drive number there.
    mov     $__lowbss_start, %di
    mov     $__lowbss_end, %cx
    sub     %di, %cx
    push    %dx
    xor     %al, %al
    rep stosb
    pop     %dx
    mov     %dl, ob_boot_drive
    sti

    call    enable_a20
    jc      die_a20

    # CPUID must exist (EFLAGS.ID toggles), and report long mode.
    pushfl
    pop     %eax
    mov     %eax, %ecx
    xor     $0x200000, %eax
    push    %eax
    popfl
    pushfl
    pop     %eax
    push    %ecx
    popfl
    xor     %ecx, %eax
    jz      die_cpu
    mov     $0x80000000, %eax
    cpuid
    cmp     $0x80000001, %eax
    jb      die_cpu
    mov     $0x80000001, %eax
    cpuid
    test    $(1 << 29), %edx
    jz      die_cpu

    # Tell the BIOS we will run in long mode (INT 15h AX=EC00h, BL=2);
    # firmware that does not know the call ignores it.
    mov     $0xec00, %ax
    mov     $2, %bl
    int     $0x15

    cli
    lgdtl   ob_gdt_desc
    mov     %cr0, %eax
    or      $1, %eax
    mov     %eax, %cr0
    ljmpl   $0x18, $pm32_first

# A20: the BIOS call first, then the fast A20 port, each followed by a
# wrap-around test. CF set on return means A20 is still off.
enable_a20:
    call    a20_on
    jnc     1f
    mov     $0x2401, %ax
    int     $0x15
    call    a20_on
    jnc     1f
    in      $0x92, %al
    test    $2, %al
    jnz     2f
    or      $2, %al
    and     $0xfe, %al              # bit 0 would reset the machine
    out     %al, $0x92
2:  mov     $0x8000, %cx
3:  call    a20_on
    jnc     1f
    loop    3b
    stc
1:  ret

# CF clear if 0000:0500 and FFFF:0510 (1 MiB higher) are different bytes.
a20_on:
    push    %ds
    push    %es
    xor     %ax, %ax
    mov     %ax, %ds
    not     %ax
    mov     %ax, %es
    movb    0x500, %bl
    movb    %es:0x510, %bh
    movb    $0x00, 0x500
    movb    $0xff, %es:0x510
    movb    0x500, %al
    movb    %bl, 0x500
    movb    %bh, %es:0x510
    pop     %es
    pop     %ds
    cmp     $0xff, %al
    clc
    jne     1f
    stc
1:  ret

die_a20:
    mov     $msg_a20, %si
    jmp     die16
die_cpu:
    mov     $msg_cpu, %si
die16:
    lodsb
    test    %al, %al
    jz      1f
    push    %si
    mov     $0x0e, %ah
    xor     %bx, %bx
    int     $0x10
    mov     $0x3f8, %dx
    out     %al, %dx
    pop     %si
    jmp     die16
1:  int     $0x18
    cli
2:  hlt
    jmp     2b

msg_a20: .asciz "OPTIMAL-BOOT: FATAL: cannot enable the A20 line\r\n"
msg_cpu: .asciz "OPTIMAL-BOOT: FATAL: this CPU is not 64-bit (no long mode)\r\n"

# Selectors: 0x08 code64, 0x10 data, 0x18 code32, 0x20 code16, 0x28 data16.
# The accessed bits are preset: otherwise the CPU sets them on the first
# load, which changes stage 2's bytes under its own CRC check.
    .balign 16
.global ob_gdt
ob_gdt:
    .quad   0
    .quad   0x00af9b000000ffff
    .quad   0x00cf93000000ffff
    .quad   0x00cf9b000000ffff
    .quad   0x00009b000000ffff
    .quad   0x000093000000ffff
ob_gdt_end:
ob_gdt_desc:
    .word   ob_gdt_end - ob_gdt - 1
    .long   ob_gdt
    .balign 8
rm_idt_desc:
    .word   0x3ff
    .long   0

.code32
# First entry into protected mode: zero .bss, build the boot page tables
# (identity map of the first 4 GiB with 2 MiB pages), go to long mode.
pm32_first:
    mov     $0x10, %ax
    mov     %ax, %ds
    mov     %ax, %es
    mov     %ax, %ss
    mov     $0x7c00, %esp
    mov     $__bss_start, %edi
    mov     $__bss_end, %ecx
    sub     %edi, %ecx
    xor     %eax, %eax
    rep stosb
    mov     $ob_boot_pdpt, %eax
    or      $3, %eax
    mov     %eax, ob_boot_pml4
    xor     %ecx, %ecx
1:  mov     %ecx, %eax
    shl     $12, %eax
    add     $ob_boot_pd, %eax
    or      $3, %eax
    mov     %eax, ob_boot_pdpt(, %ecx, 8)
    inc     %ecx
    cmp     $4, %ecx
    jb      1b
    xor     %ecx, %ecx
2:  mov     %ecx, %eax
    shl     $21, %eax
    or      $0x83, %eax             # present, writable, 2 MiB
    mov     %eax, ob_boot_pd(, %ecx, 8)
    mov     %ecx, %eax
    shr     $11, %eax               # bits 32+ of the address: index / 2048
    mov     %eax, ob_boot_pd + 4(, %ecx, 8)
    inc     %ecx
    cmp     $2048, %ecx
    jb      2b
    movl    $ob_boot_pml4, ob_cr3
    movl    $0, ob_cr3 + 4
    movb    $0, ob_resume
    jmp     pm32_to_long

# Back from real mode (or the first time): paging off, PE on, flat data.
pm32_to_long:
    mov     %cr4, %eax
    or      $0x20, %eax             # PAE
    mov     %eax, %cr4
    mov     ob_cr3, %eax
    mov     %eax, %cr3
    mov     $0xc0000080, %ecx       # EFER
    rdmsr
    or      $0x100, %eax            # LME
    wrmsr
    mov     %cr0, %eax
    or      $0x80000001, %eax       # PG | PE
    and     $0x9fffffff, %eax       # caches on (CD, NW clear)
    mov     %eax, %cr0
    ljmpl   $0x08, $lm64

.code64
lm64:
    mov     $0x10, %ax
    mov     %ax, %ds
    mov     %ax, %es
    mov     %ax, %ss
    xor     %ax, %ax
    mov     %ax, %fs
    mov     %ax, %gs
    cmpb    $0, ob_resume
    jne     lm64_resume
    movb    $1, ob_resume
    lea     __stack_top(%rip), %rsp
    movzbl  ob_boot_drive, %edi
    call    stage2_main
1:  cli
    hlt
    jmp     1b

lm64_resume:
    mov     ob_saved_rsp, %rsp
    lidt    ob_idt_desc
    pop     %r15
    pop     %r14
    pop     %r13
    pop     %r12
    pop     %rbp
    pop     %rbx
    ret

# extern "sysv64" fn ob_bios_int(vector: u32)
# Runs INT <vector> in real mode with the registers in ob_regs, and stores
# the registers the BIOS returns back there. Interrupts are on only while
# in real mode.
.global ob_bios_int
ob_bios_int:
    push    %rbx
    push    %rbp
    push    %r12
    push    %r13
    push    %r14
    push    %r15
    mov     %dil, rm_int_vector + 1
    mov     %rsp, ob_saved_rsp
    push    $0x20
    push    $pm16
    lretq

.code16
# 16-bit protected mode (compatibility mode, still in long mode).
pm16:
    mov     $0x28, %ax
    mov     %ax, %ds
    mov     %ax, %es
    mov     %ax, %ss
    mov     %ax, %fs
    mov     %ax, %gs
    mov     %cr0, %eax
    and     $0x7fffffff, %eax       # paging off: leaves long mode
    mov     %eax, %cr0
    mov     $0xc0000080, %ecx
    rdmsr
    and     $0xfffffeff, %eax       # LME off
    wrmsr
    mov     %cr0, %eax
    and     $0xfffffffe, %eax       # PE off
    mov     %eax, %cr0
    ljmp    $0, $rm16

rm16:
    xor     %ax, %ax
    mov     %ax, %ds
    mov     %ax, %ss
    mov     %ax, %fs
    mov     %ax, %gs
    mov     $0x7c00, %sp
    lidt    rm_idt_desc
    mov     ob_regs + 34, %ax       # es
    mov     %ax, %es
    mov     ob_regs + 0, %eax
    mov     ob_regs + 4, %ebx
    mov     ob_regs + 8, %ecx
    mov     ob_regs + 12, %edx
    mov     ob_regs + 16, %esi
    mov     ob_regs + 20, %edi
    mov     ob_regs + 24, %ebp
    pushw   ob_regs + 32            # ds
    pop     %ds
    sti
rm_int_vector:
    int     $0x00                   # vector patched by ob_bios_int
    cli
    pushfl
    push    %ds
    push    %ax
    xor     %ax, %ax
    mov     %ax, %ds
    pop     %ax
    popw    ob_regs + 32            # ds
    popl    ob_regs + 28            # eflags
    mov     %eax, ob_regs + 0
    mov     %ebx, ob_regs + 4
    mov     %ecx, ob_regs + 8
    mov     %edx, ob_regs + 12
    mov     %esi, ob_regs + 16
    mov     %edi, ob_regs + 20
    mov     %ebp, ob_regs + 24
    mov     %es, %ax
    mov     %ax, ob_regs + 34
    xor     %ax, %ax
    mov     %ax, %es
    cld
    lgdtl   ob_gdt_desc
    mov     %cr0, %eax
    or      $1, %eax
    mov     %eax, %cr0
    ljmpl   $0x18, $pm32_back

.code32
pm32_back:
    mov     $0x10, %ax
    mov     %ax, %ds
    mov     %ax, %es
    mov     %ax, %ss
    jmp     pm32_to_long

# Variables shared with real mode.
.section .lowbss, "aw", @nobits
.global ob_regs
    .balign 8
ob_regs:        .skip 40            # eax ebx ecx edx esi edi ebp eflags (u32), ds es (u16)
.global ob_boot_drive
ob_boot_drive:  .skip 1
ob_resume:      .skip 1
    .balign 8
ob_saved_rsp:   .skip 8
.global ob_cr3
ob_cr3:         .skip 8
.global ob_idt_desc
ob_idt_desc:    .skip 16
.global ob_low_buf
    .balign 16
ob_low_buf:     .skip 1024          # BIOS call buffers (DAP, E820, drive params, VBE)

.section .bss.boot_tables, "aw", @nobits
    .balign 4096
ob_boot_pml4:   .skip 4096
ob_boot_pdpt:   .skip 4096
ob_boot_pd:     .skip 4 * 4096
