use optimalfs::sim::Rng;
use optimalfs::*;

fn fmt(blocks: u64, enc: bool) -> Filesystem<MemDevice> {
    let mut rng = Rng::new(1);
    let opts = FormatOptions {
        passphrase: enc.then(|| b"pw".to_vec()),
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
    };
    Filesystem::format(MemDevice::new(blocks), &opts, &mut rng, Box::new(ZeroClock)).unwrap()
}

#[test]
fn basic_roundtrip() {
    for enc in [false, true] {
        let mut fs = fmt(4096, enc);
        let d = fs.mkdir(ROOT_INO, b"dir", 0o755).unwrap();
        let f = fs.create(d, b"file", 0o644).unwrap();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        fs.write(f, 100, &data).unwrap();
        fs.commit().unwrap();
        let mut buf = vec![0u8; data.len()];
        assert_eq!(fs.read(f, 100, &mut buf).unwrap(), data.len());
        assert_eq!(buf, data);
        let dev = fs.into_device();
        let mo = MountOptions {
            passphrase: enc.then(|| b"pw".to_vec()),
            ..Default::default()
        };
        let mut fs = Filesystem::mount(dev, &mo, &mut Rng::new(2), Box::new(ZeroClock)).unwrap();
        let d2 = fs.lookup(ROOT_INO, b"dir").unwrap();
        let f2 = fs.lookup(d2, b"file").unwrap();
        let mut buf = vec![0u8; data.len() + 100];
        assert_eq!(fs.read(f2, 0, &mut buf).unwrap(), data.len() + 100);
        assert_eq!(&buf[100..], &data[..]);
        assert!(buf[..100].iter().all(|&b| b == 0));
        for i in 0..500 {
            let name = format!("f{i}");
            fs.create(d2, name.as_bytes(), 0o644).unwrap();
        }
        fs.commit().unwrap();
        assert_eq!(fs.readdir(d2).unwrap().len(), 501);
        for i in 0..500 {
            fs.unlink(d2, format!("f{i}").as_bytes()).unwrap();
        }
        fs.unlink(d2, b"file").unwrap();
        fs.commit().unwrap();
        fs.rmdir(ROOT_INO, b"dir").unwrap();
        fs.commit().unwrap();
        fsck_ok(&mut fs);
        let st = fs.statfs();
        println!("enc={enc} {st:?} {:?}", fs.io_stats());
    }
}

fn fsck_ok<D: BlockDevice>(fs: &mut Filesystem<D>) {
    let r = optimalfs::fsck::check(fs, true).unwrap();
    assert!(r.ok(), "fsck errors: {:#?}", r.errors);
}

#[test]
fn snapshots_and_rollback() {
    for enc in [false, true] {
        let mut fs = fmt(8192, enc);
        let f = fs.create(ROOT_INO, b"a", 0o644).unwrap();
        fs.write(f, 0, &vec![1u8; 50_000]).unwrap();
        fs.commit().unwrap();
        fsck_ok(&mut fs);
        let s1 = fs.snapshot_create(b"one").unwrap();
        fsck_ok(&mut fs);
        fs.write(f, 10_000, &vec![2u8; 10_000]).unwrap();
        let g = fs.create(ROOT_INO, b"b", 0o644).unwrap();
        fs.write(g, 0, &vec![3u8; 9000]).unwrap();
        fs.commit().unwrap();
        fsck_ok(&mut fs);
        let s2 = fs.snapshot_create(b"two").unwrap();
        fs.unlink(ROOT_INO, b"a").unwrap();
        fs.commit().unwrap();
        fsck_ok(&mut fs);
        let s3 = fs.snapshot_create(b"three").unwrap();
        fs.truncate(g, 100).unwrap();
        fs.commit().unwrap();
        fsck_ok(&mut fs);
        fs.snapshot_delete(s2).unwrap();
        fsck_ok(&mut fs);
        let free_before = fs.statfs().free_blocks;
        fs.rollback(s1).unwrap();
        fsck_ok(&mut fs);
        assert!(fs.snapshot_list().unwrap().iter().map(|s| s.id).eq([s1]));
        let a = fs.lookup(ROOT_INO, b"a").unwrap();
        let mut buf = vec![0u8; 50_000];
        fs.read(a, 0, &mut buf).unwrap();
        assert!(buf.iter().all(|&x| x == 1));
        assert_eq!(fs.lookup(ROOT_INO, b"b"), Err(Error::NotFound));
        let _ = (s3, free_before);
        fs.snapshot_delete(s1).unwrap();
        fsck_ok(&mut fs);
        fs.unlink(ROOT_INO, b"a").unwrap();
        fs.commit().unwrap();
        fsck_ok(&mut fs);
    }
}
