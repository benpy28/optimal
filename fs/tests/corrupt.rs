//! Damaged and hostile images: random bit flips, zeroed blocks, garbage,
//! misdirected writes (a block copied over another) and stale blocks (an
//! old version put back). Mounting and using such an image must never
//! panic, hang or return wrong data: every read either fails with an error
//! or returns exactly what some committed state held.

mod common;
use common::*;
use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Builds a populated volume; returns the device, the committed states and an older image.
fn build(seed: u64, enc: bool) -> (CrashDevice, std::collections::BTreeMap<u64, Model>, Vec<u8>) {
    let mut h = Harness::new(3000, enc, seed).unwrap();
    let mut r = Rng::new(seed);
    let cfg = GenCfg {
        max_write: 50_000,
        max_offset: 300_000,
        remounts: false,
        ..Default::default()
    };
    let mut old = None;
    for i in 0..150 {
        let op = gen_op(&mut r, &h.model, &cfg);
        h.apply(&op).unwrap();
        if i == 75 {
            h.apply(&Op::Commit).unwrap();
            old = Some(h.fs().device().clean_image());
        }
    }
    h.apply(&Op::Commit).unwrap();
    let committed = h.committed.clone();
    (h.fs.take().unwrap().into_device(), committed, old.unwrap())
}

fn mutate(r: &mut Rng, img: &mut [u8], old: &[u8], live: &[usize]) {
    let blocks = img.len() / 4096;
    for _ in 0..1 + r.below(4) {
        // Mostly hit blocks that hold something; unused blocks rarely matter.
        let b = if r.chance(9, 10) {
            live[r.below(live.len() as u64) as usize]
        } else {
            r.below(blocks as u64) as usize
        };
        let blk = b * 4096..(b + 1) * 4096;
        match r.below(6) {
            0 => {
                for _ in 0..1 + r.below(8) {
                    let i = blk.start + r.below(4096) as usize;
                    img[i] ^= 1 << r.below(8);
                }
            }
            1 => img[blk].fill(0),
            2 => r.fill(&mut img[blk]),
            3 => {
                let src = r.below(blocks as u64) as usize * 4096;
                let copy = img[src..src + 4096].to_vec();
                img[blk].copy_from_slice(&copy);
            }
            4 => img[blk.clone()].copy_from_slice(&old[blk]),
            _ => {
                // Torn: one sector from the old image.
                let s = blk.start + r.below(8) as usize * 512;
                img[s..s + 512].copy_from_slice(&old[s..s + 512]);
            }
        }
    }
}

/// Uses the volume read-only; any data that reads back must match `want`.
fn exercise(fs: &mut Filesystem<CrashDevice>, want: &MFs) {
    for (&ino, n) in &want.nodes {
        match n.kind {
            FileKind::File => {
                let mut buf = vec![0u8; n.data.len()];
                if let Ok(len) = fs.read(ino, 0, &mut buf) {
                    assert_eq!(
                        len,
                        n.data.len(),
                        "read returned a wrong length instead of an error"
                    );
                    assert!(
                        buf == n.data,
                        "read returned wrong data instead of an error"
                    );
                }
            }
            FileKind::Dir => {
                if let Ok(es) = fs.readdir(ino) {
                    let mut got: Vec<(Vec<u8>, u64)> =
                        es.into_iter().map(|e| (e.name, e.ino)).collect();
                    got.sort();
                    let w: Vec<(Vec<u8>, u64)> =
                        n.entries.iter().map(|(k, v)| (k.clone(), *v)).collect();
                    assert_eq!(got, w, "readdir returned wrong entries instead of an error");
                }
            }
            FileKind::Symlink => {
                if let Ok(t) = fs.readlink(ino) {
                    assert_eq!(t, n.target);
                }
            }
        }
    }
}

/// Random operations with results ignored: only panics count.
fn chaos(fs: &mut Filesystem<CrashDevice>, m: &Model, seed: u64) {
    let mut r = Rng::new(seed);
    let cfg = GenCfg {
        remounts: false,
        ..Default::default()
    };
    for _ in 0..25 {
        match gen_op(&mut r, m, &cfg) {
            Op::Create(d, n, p) => drop(fs.create(d, &n, p)),
            Op::Mkdir(d, n, p) => drop(fs.mkdir(d, &n, p)),
            Op::Symlink(d, n, t) => drop(fs.symlink(d, &n, &t)),
            Op::Link(i, d, n) => drop(fs.link(i, d, &n)),
            Op::Unlink(d, n) => drop(fs.unlink(d, &n)),
            Op::Rmdir(d, n) => drop(fs.rmdir(d, &n)),
            Op::Rename(a, b, c, d) => drop(fs.rename(a, &b, c, &d)),
            Op::Write(i, o, l, s) => drop(fs.write(i, o, &content(s, l))),
            Op::Truncate(i, s) => drop(fs.truncate(i, s)),
            Op::SetAttr(i, s) => drop(fs.setattr(i, &s)),
            Op::Commit => drop(fs.commit()),
            Op::Snapshot => drop(fs.snapshot_create(b"x")),
            Op::SnapDelete(id) => drop(fs.snapshot_delete(id)),
            Op::Rollback(id) => drop(fs.rollback(id)),
            Op::Remount => {}
            Op::Fsync(i) => drop(fs.fsync(i)),
        }
    }
    let _ = fs.commit();
    let _ = fsck::check(fs, false);
}

fn campaign(seed: u64, enc: bool, iters: u64) {
    let (dev, committed, old) = build(seed, enc);
    let good = dev.clean_image();
    let mut r = Rng::new(seed ^ 0xbad);
    let live: Vec<usize> = (0..good.len() / 4096)
        .filter(|&b| good[b * 4096..(b + 1) * 4096].iter().any(|&x| x != 0))
        .collect();
    let (mut mounted, mut failed, mut fell_back, mut healed) = (0, 0, 0, 0u64);
    for it in 0..iters {
        let mut img = good.clone();
        mutate(&mut r, &mut img, &old, &live);
        let fell_back_flag = std::cell::Cell::new(false);
        let healed_flag = std::cell::Cell::new(false);
        let res = catch_unwind(AssertUnwindSafe(|| {
            let d = CrashDevice::from_image(img, it);
            match Filesystem::mount(d, &mount_opts(enc), &mut Rng::new(it), Box::new(ZeroClock)) {
                Err(_) => false,
                Ok(mut fs) => {
                    let g = fs.generation();
                    if g != *committed.keys().next_back().unwrap() {
                        fell_back_flag.set(true);
                    }
                    let want = committed
                        .get(&g)
                        .expect("mounted a generation that was never committed")
                        .clone();
                    exercise(&mut fs, &want.live);
                    let _ = fsck::check(&mut fs, true);
                    // A bad metadata copy whose twin was good: found and
                    // survived, which is detection too.
                    if fs.io_stats().bad_copies > 0 {
                        healed_flag.set(true);
                    }
                    // Writing to a damaged volume may fail but must not panic.
                    chaos(&mut fs, &want, it);
                    true
                }
            }
        }));
        match res {
            Ok(true) => {
                mounted += 1;
                fell_back += fell_back_flag.get() as u64;
                healed += healed_flag.get() as u64;
            }
            Ok(false) => failed += 1,
            Err(_) => panic!("seed {seed:#x} iteration {it}: panic on a damaged image"),
        }
    }
    eprintln!(
        "seed {seed:#x} enc {enc}: {mounted} mounted ({fell_back} on an older commit, {healed} with a bad copy found and survived), {failed} refused"
    );
    // The campaign must actually reach the interesting paths. With
    // metadata stored twice, most single-block damage is caught as a bad
    // copy and survived, so that counts as reaching them.
    assert!(
        failed > 0 || fell_back > 0 || healed > 0,
        "no mutation was ever detected at mount; the test is too weak"
    );
}

#[test]
fn damaged_images_plain() {
    for i in 0..scale() {
        campaign(base_seed() + 50_000 + i, false, 300);
    }
}

#[test]
fn damaged_images_encrypted() {
    for i in 0..scale() {
        campaign(base_seed() + 60_000 + i, true, 300);
    }
}
