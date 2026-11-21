//! Build script: points the linker at linker.ld and generates the 256
//! interrupt entry stubs, so no stub is written (or mistyped) by hand.

use std::fmt::Write as _;

/// Size of each interrupt entry stub (must match `idt::STUB_SIZE`).
const STUB_SIZE: usize = 16;

/// Exceptions where the CPU pushes an error code itself (Intel SDM Vol. 3A, 6.13).
const HAS_ERROR_CODE: [u8; 10] = [8, 10, 11, 12, 13, 14, 17, 21, 29, 30];

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    println!("cargo:rustc-link-search={dir}");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=build.rs");

    // Every stub sits in its own 16-byte slot (at most 12 bytes of code,
    // checked by the `isr_stubs_are_laid_out_as_expected` kernel test), so
    // stub N lives at isr_stub_0 + 16 * N. The IDT computes addresses from that instead of
    // reading a table of absolute pointers, which keeps the kernel free of
    // relocations in read-only memory (it is position independent for KASLR).
    let mut asm = String::new();
    writeln!(
        asm,
        ".section .text.isr, \"ax\"\n.balign {STUB_SIZE}\n.global isr_stub_0"
    )
    .unwrap();
    for v in 0..=255u8 {
        writeln!(asm, "isr_stub_{v}:").unwrap();
        if !HAS_ERROR_CODE.contains(&v) {
            // Push a dummy error code so every frame has the same layout.
            asm.push_str("    push 0\n");
        }
        // `jmp rel32` spelled out, so every stub has the same fixed encoding
        // whatever the distance to isr_common.
        writeln!(
            asm,
            "    push {v}\n    .byte 0xe9\n    .long isr_common - . - 4"
        )
        .unwrap();
        writeln!(asm, "    .balign {STUB_SIZE}, 0xcc").unwrap();
    }
    std::fs::write(format!("{out}/isr_stubs.s"), asm).unwrap();
}
