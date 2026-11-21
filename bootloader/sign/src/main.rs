//! optimal-boot-sign: keys, hashes and config signatures for Optimal Boot.
//!
//!   optimal-boot-sign keygen <secret-key-file> <public-key-file>
//!   optimal-boot-sign hash <file>
//!   optimal-boot-sign sign <secret-key-file> <config>      (writes <config>.sig)
//!   optimal-boot-sign verify <public-key-file> <config>
//!
//! Keys are hex text files. `sign` refuses configs the loader would refuse.

use ed25519_dalek::{Signer, SigningKey};
use optimal_boot_core::{config, sig};
use std::io::Read;
use std::process::exit;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn read_key(path: &str) -> [u8; 32] {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("");
    config::parse_hex::<32>(line).unwrap_or_else(|| die(&format!("{path}: not a 64-hex-digit key")))
}

fn die(msg: &str) -> ! {
    eprintln!("optimal-boot-sign: {msg}");
    exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("keygen") if args.len() == 4 => {
            let mut seed = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut seed))
                .unwrap_or_else(|e| die(&format!("/dev/urandom: {e}")));
            let key = SigningKey::from_bytes(&seed);
            std::fs::write(&args[2], format!("{}\n", hex(&seed)))
                .unwrap_or_else(|e| die(&e.to_string()));
            std::fs::write(
                &args[3],
                format!("{}\n", hex(&key.verifying_key().to_bytes())),
            )
            .unwrap_or_else(|e| die(&e.to_string()));
        }
        Some("hash") if args.len() == 3 => {
            let data =
                std::fs::read(&args[2]).unwrap_or_else(|e| die(&format!("{}: {e}", args[2])));
            println!("{}", hex(&sig::sha512(&data)));
        }
        Some("sign") if args.len() == 4 => {
            let key = SigningKey::from_bytes(&read_key(&args[2]));
            let cfg = std::fs::read(&args[3]).unwrap_or_else(|e| die(&format!("{}: {e}", args[3])));
            if let Err(e) = config::parse(&cfg) {
                die(&format!(
                    "{} line {}: {}",
                    args[3],
                    e.line,
                    e.kind.describe()
                ));
            }
            let s = key.sign(&sig::signed_message(&cfg));
            let mut out = sig::MAGIC.to_vec();
            out.extend_from_slice(&sig::key_id(&key.verifying_key().to_bytes()));
            out.extend_from_slice(&s.to_bytes());
            std::fs::write(format!("{}.sig", args[3]), out).unwrap_or_else(|e| die(&e.to_string()));
        }
        Some("verify") if args.len() == 4 => {
            let key = read_key(&args[2]);
            let cfg = std::fs::read(&args[3]).unwrap_or_else(|e| die(&e.to_string()));
            let s =
                std::fs::read(format!("{}.sig", args[3])).unwrap_or_else(|e| die(&e.to_string()));
            match sig::verify(&cfg, &s, &[key]) {
                Ok(_) => println!("signature valid"),
                Err(e) => die(e.describe()),
            }
        }
        _ => die(
            "usage: keygen <secret> <public> | hash <file> | sign <secret> <config> | verify <public> <config>",
        ),
    }
}
