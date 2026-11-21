//! Build script: points the linker at linker.ld and generates the 256
//! interrupt entry stubs, so no stub is written (or mistyped) by hand.

use std::fmt::Write as _;

/// Exceptions where the CPU pushes an error code itself (Intel SDM Vol. 3A, 6.13).
const HAS_ERROR_CODE: [u8; 10] = [8, 10, 11, 12, 13, 14, 17, 21, 29, 30];

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    println!("cargo:rustc-link-search={dir}");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=build.rs");

    let mut asm = String::new();
    asm.push_str(".section .text.isr, \"ax\"\n");
    for v in 0..=255u8 {
        writeln!(asm, ".global isr_stub_{v}\n.p2align 4\nisr_stub_{v}:").unwrap();
        if !HAS_ERROR_CODE.contains(&v) {
            // Push a dummy error code so every frame has the same layout.
            asm.push_str("    push 0\n");
        }
        writeln!(asm, "    push {v}\n    jmp isr_common").unwrap();
    }
    asm.push_str(
        ".section .rodata.isr, \"a\"\n.global isr_stub_table\n.p2align 3\nisr_stub_table:\n",
    );
    for v in 0..=255u8 {
        writeln!(asm, "    .quad isr_stub_{v}").unwrap();
    }
    std::fs::write(format!("{out}/isr_stubs.s"), asm).unwrap();
}
