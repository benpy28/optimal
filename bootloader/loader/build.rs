//! Embeds the trusted config-signing public key(s) into the loader
//! (shared with the BIOS stage: ../build-support/keys.rs).
#[path = "../build-support/keys.rs"]
mod keys;

fn main() {
    keys::write_trusted_keys();
}
