// Shared by the loader's and the BIOS stage's build scripts (included with
// #[path]): embeds the trusted config-signing public keys.
//
// OPTIMAL_BOOT_PUBKEYS is a path to a file with one 64-hex-digit Ed25519
// public key per line. There is deliberately no way to build a loader that
// skips signature checks.
use std::fmt::Write;

pub fn write_trusted_keys() {
    println!("cargo:rerun-if-env-changed=OPTIMAL_BOOT_PUBKEYS");
    let path = std::env::var("OPTIMAL_BOOT_PUBKEYS").unwrap_or_else(|_| {
        panic!("set OPTIMAL_BOOT_PUBKEYS to a file of trusted Ed25519 public keys (hex, one per line); `optimal-boot-sign keygen` makes one")
    });
    println!("cargo:rerun-if-changed={path}");
    let text = std::fs::read_to_string(&path).expect("read OPTIMAL_BOOT_PUBKEYS");
    let mut keys = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        assert!(
            line.len() == 64 && line.bytes().all(|c| c.is_ascii_hexdigit()),
            "bad key line: {line}"
        );
        let bytes: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&line[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        keys.push(bytes);
    }
    assert!(
        !keys.is_empty() && keys.len() <= 8,
        "need 1 to 8 trusted keys"
    );
    let mut out = format!("pub static TRUSTED_KEYS: [[u8; 32]; {}] = [\n", keys.len());
    for k in &keys {
        out.push_str("    [");
        for b in k {
            write!(out, "{b:#04x}, ").unwrap();
        }
        out.push_str("],\n");
    }
    out.push_str("];\n");
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("keys.rs");
    std::fs::write(dest, out).unwrap();
}
