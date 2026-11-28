# Optimal Boot, BIOS stage 1: the 440 bytes of code in the MBR.
#
# The BIOS loads this sector at 0x7C00 and jumps to it with the boot drive
# in DL. It loads stage 2 (installer-recorded LBA and sector count, below)
# to 0x8000 with the INT 13h extensions, checks stage 2's magic, and jumps
# to its entry at 0x8040 with the drive still in DL. Errors print a short
# message to the screen and COM1 and return to the BIOS with INT 18h, so it
# can try the next boot device.
#
# Bytes 440-511 of the sector (disk signature, partition table, 55AA) are
# never written by the installer, so they are not part of this section.

.section .stage1, "awx"
.code16
.global stage1_start
stage1_start:
    cli
    ljmp    $0, $s1_norm            # some BIOSes enter at 07C0:0000
s1_norm:
    xor     %ax, %ax
    mov     %ax, %ds
    mov     %ax, %es
    mov     %ax, %ss
    mov     $0x7c00, %sp
    sti
    cld
    mov     %dl, s1_drive

    # The INT 13h extensions (fixed disk access subset) are required.
    mov     $0x41, %ah
    mov     $0x55aa, %bx
    int     $0x13
    jc      s1_no_ext
    cmp     $0xaa55, %bx
    jne     s1_no_ext
    test    $1, %cx
    jz      s1_no_ext

    mov     s1_sectors, %cx
    test    %cx, %cx
    jz      s1_not_installed
    movw    $0x0800, s1_dap_seg
s1_loop:
    mov     %cx, %ax
    cmp     $64, %ax                # 32 KiB per request (512-byte sectors)
    jbe     1f
    mov     $64, %ax
1:  mov     %ax, s1_dap_count
    push    %cx
    push    %ax
    mov     $3, %di                 # tries
2:  mov     $0x42, %ah
    mov     s1_drive, %dl
    mov     $s1_dap, %si
    int     $0x13
    jnc     3f
    xor     %ah, %ah                # reset the drive, then retry
    mov     s1_drive, %dl
    int     $0x13
    dec     %di
    jnz     2b
    jmp     s1_disk_error
3:  pop     %ax
    pop     %cx
    sub     %ax, %cx
    add     %ax, s1_dap_lba
    adcw    $0, s1_dap_lba + 2
    adcw    $0, s1_dap_lba + 4
    adcw    $0, s1_dap_lba + 6
    shl     $5, %ax                 # sectors * 512 / 16 = paragraphs
    add     %ax, s1_dap_seg
    test    %cx, %cx
    jnz     s1_loop

    # Stage 2 starts with "OBSTAGE2".
    mov     $0x8000, %si
    mov     $s1_magic, %di
    mov     $8, %cx
    repe cmpsb
    jne     s1_bad_stage2
    mov     s1_drive, %dl
    ljmp    $0, $0x8040

s1_no_ext:
    mov     $s1_msg_ext, %si
    jmp     s1_fail
s1_not_installed:
    mov     $s1_msg_inst, %si
    jmp     s1_fail
s1_disk_error:
    mov     $s1_msg_disk, %si
    jmp     s1_fail
s1_bad_stage2:
    mov     $s1_msg_bad, %si
s1_fail:
    push    %si
    mov     $s1_msg_pre, %si
    call    s1_puts
    pop     %si
    call    s1_puts
    int     $0x18                   # next boot device
    cli
1:  hlt
    jmp     1b

s1_puts:
    lodsb
    test    %al, %al
    jz      2f
    push    %si
    mov     $0x0e, %ah              # teletype
    xor     %bx, %bx
    int     $0x10
    mov     $0x3f8, %dx             # and COM1
    out     %al, %dx
    pop     %si
    jmp     s1_puts
2:  ret

s1_msg_pre:   .asciz "OPTIMAL-BOOT: FATAL: stage 1: "
s1_msg_ext:   .asciz "no INT 13h extensions\r\n"
s1_msg_inst:  .asciz "not installed\r\n"
s1_msg_disk:  .asciz "disk read failed\r\n"
s1_msg_bad:   .asciz "stage 2 missing\r\n"
s1_magic:     .ascii "OBSTAGE2"
s1_drive:     .byte 0

    # The disk address packet, with the installer's fields (DESIGN.md,
    # BIOS stage) at fixed offsets: stage 2's first LBA at 0x1A8 and its
    # length in 512-byte sectors at 0x1B0.
    .org    0x1a0
s1_dap:
    .byte   16, 0
s1_dap_count:
    .word   0
s1_dap_off:
    .word   0
s1_dap_seg:
    .word   0
s1_dap_lba:
    .quad   0
s1_sectors:
    .word   0
    .org    0x1b8
