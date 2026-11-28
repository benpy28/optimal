//! optimal-boot-install: puts Optimal Boot's BIOS stage on a disk.
//!
//!   optimal-boot-install bios --image <optimal-boot-bios ELF> --disk <file or device>
//!                             --partition <N> [--sector-size 512|4096]
//!
//! Partition N (1-based) is the boot volume: the FAT file system holding
//! /boot/optimal-boot.conf. Stage 2 goes into the BIOS boot partition on a
//! GPT disk (type 21686148-6449-6E6F-744E-656564454649), or into the gap
//! between the MBR and the first partition on an MBR disk. Stage 1 goes
//! into the first 440 bytes of sector 0; the disk signature and the
//! partition table after them are never written.
//!
//! Everything is checked before anything is written, and the writes are
//! read back and compared.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::process::ExitCode;

mod plan;

fn usage() -> ExitCode {
    eprintln!(
        "usage: optimal-boot-install bios --image <optimal-boot-bios ELF> --disk <path> --partition <N> [--sector-size 512|4096]"
    );
    ExitCode::from(2)
}

/// A disk image or device opened read-write.
struct FileDisk {
    f: std::fs::File,
    len: u64,
}

impl plan::Disk for FileDisk {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<(), String> {
        self.f
            .seek(SeekFrom::Start(off))
            .map_err(|e| e.to_string())?;
        self.f
            .read_exact(buf)
            .map_err(|e| format!("read at {off}: {e}"))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("bios") {
        return usage();
    }
    let mut image = None;
    let mut disk = None;
    let mut partition = None;
    let mut ss = 512usize;
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned();
        match (args[i].as_str(), v) {
            ("--image", Some(v)) => image = Some(v),
            ("--disk", Some(v)) => disk = Some(v),
            ("--partition", Some(v)) => partition = v.parse::<u32>().ok(),
            ("--sector-size", Some(v)) => match v.parse() {
                Ok(n @ (512 | 4096)) => ss = n,
                _ => return usage(),
            },
            _ => return usage(),
        }
        i += 2;
    }
    let (Some(image), Some(disk_path), Some(partition)) = (image, disk, partition) else {
        return usage();
    };
    let elf = match std::fs::read(&image) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{image}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let stages = match plan::stages_from_elf(&elf) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{image}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let f = match OpenOptions::new().read(true).write(true).open(&disk_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{disk_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let len = match (&f).seek(SeekFrom::End(0)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{disk_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut d = FileDisk { f, len };
    let p = match plan::plan(&mut d, &stages, partition, ss) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{disk_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    for w in &p.writes {
        let r =
            d.f.seek(SeekFrom::Start(w.offset))
                .and_then(|_| d.f.write_all(&w.bytes));
        if let Err(e) = r {
            eprintln!("{disk_path}: write at {}: {e}", w.offset);
            return ExitCode::FAILURE;
        }
    }
    if let Err(e) = d.f.sync_all() {
        eprintln!("{disk_path}: {e}");
        return ExitCode::FAILURE;
    }
    for w in &p.writes {
        let mut back = vec![0u8; w.bytes.len()];
        if plan::Disk::read_at(&mut d, w.offset, &mut back).is_err() || back != w.bytes {
            eprintln!(
                "{disk_path}: read-back at {} does not match what was written",
                w.offset
            );
            return ExitCode::FAILURE;
        }
    }
    println!("{}", p.summary);
    ExitCode::SUCCESS
}
