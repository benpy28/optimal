//! Embeds the trusted config-signing keys (the same file format and the
//! same check as the UEFI loader) and links stage 1 and 2 with link.ld.
#[path = "../build-support/keys.rs"]
mod keys;

fn main() {
    keys::write_trusted_keys();
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=-T{dir}/link.ld");
    println!("cargo:rustc-link-arg-bins=--no-pie");
    println!("cargo:rerun-if-changed={dir}/link.ld");
    println!("cargo:rerun-if-changed={dir}/src/stage1.s");
    println!("cargo:rerun-if-changed={dir}/src/stage2.s");
}
