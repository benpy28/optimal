//! The boot configuration file. It is signed as a whole (see `sig`), and it
//! pins the SHA-512 of every file it loads, so one signature covers the
//! complete boot: which kernels, which modules, and which command lines.
//!
//! Format, one statement per line, `#` starts a comment:
//!
//! ```text
//! optimal-boot-config 1
//! timeout 3
//! default optimal
//! kaslr on
//!
//! entry optimal
//!   title Optimal
//!   kernel /boot/optimal-kernel sha512:<128 hex digits>
//!   cmdline quiet
//!   module /boot/initrd sha512:<hex> optional string
//! ```
//!
//! Parsing is strict: unknown keys, bad values and duplicates are errors,
//! because a silently ignored line in a security-critical file is a bug.

pub const MAX_ENTRIES: usize = 16;
pub const MAX_MODULES: usize = 16;
pub const MAX_PATH: usize = 255;
pub const MAX_CMDLINE: usize = 4096;
pub const MAX_TITLE: usize = 64;
pub const MAX_ID: usize = 32;
pub const MAX_TIMEOUT: u32 = 60;
pub const MAX_CONFIG_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub line: usize,
    pub kind: ErrorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    TooLarge,
    NotText,
    MissingHeader,
    UnsupportedVersion,
    UnknownKey,
    MissingValue,
    BadValue,
    Duplicate,
    OutsideEntry,
    TooManyEntries,
    TooManyModules,
    BadPath,
    BadHash,
    MissingKernel,
    NoEntries,
    UnknownDefault,
}

impl ErrorKind {
    pub fn describe(&self) -> &'static str {
        match self {
            ErrorKind::TooLarge => "config file is larger than 64 KiB",
            ErrorKind::NotText => "config file contains characters other than printable ASCII",
            ErrorKind::MissingHeader => "first statement must be `optimal-boot-config 1`",
            ErrorKind::UnsupportedVersion => "config format version is not supported",
            ErrorKind::UnknownKey => "unknown statement",
            ErrorKind::MissingValue => "statement needs a value",
            ErrorKind::BadValue => "invalid value",
            ErrorKind::Duplicate => "statement given twice",
            ErrorKind::OutsideEntry => "statement belongs inside an entry",
            ErrorKind::TooManyEntries => "too many entries (at most 16)",
            ErrorKind::TooManyModules => "too many modules in one entry (at most 16)",
            ErrorKind::BadPath => "file paths must start with / and use plain ASCII without ..",
            ErrorKind::BadHash => "file hash must be sha512: followed by 128 hex digits",
            ErrorKind::MissingKernel => "entry has no kernel",
            ErrorKind::NoEntries => "config has no entries",
            ErrorKind::UnknownDefault => "default names an entry that does not exist",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileRef<'a> {
    pub path: &'a str,
    pub sha512: [u8; 64],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Module<'a> {
    pub file: FileRef<'a>,
    pub string: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub kernel: Option<FileRef<'a>>,
    pub cmdline: &'a str,
    modules: [Option<Module<'a>>; MAX_MODULES],
    module_count: usize,
    pub line: usize,
}

impl<'a> Entry<'a> {
    pub fn modules(&self) -> impl Iterator<Item = &Module<'a>> {
        self.modules[..self.module_count].iter().flatten()
    }
    pub fn module_count(&self) -> usize {
        self.module_count
    }
    pub fn kernel(&self) -> FileRef<'a> {
        self.kernel.expect("validated")
    }
}

#[derive(Debug)]
pub struct Config<'a> {
    pub timeout: u32,
    pub default: usize,
    pub kaslr: bool,
    pub resolution: Option<(u32, u32)>,
    /// Security version number, for anti-rollback (enforced in a later stage).
    pub svn: u64,
    entries: [Option<Entry<'a>>; MAX_ENTRIES],
    entry_count: usize,
}

impl<'a> Config<'a> {
    pub fn entries(&self) -> impl Iterator<Item = &Entry<'a>> {
        self.entries[..self.entry_count].iter().flatten()
    }
    pub fn entry(&self, i: usize) -> &Entry<'a> {
        self.entries[i].as_ref().unwrap()
    }
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }
}

pub fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = s.as_bytes();
    if b.len() != N * 2 {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for i in 0..N {
        out[i] = nib(b[2 * i])? << 4 | nib(b[2 * i + 1])?;
    }
    Some(out)
}

fn valid_path(p: &str) -> bool {
    p.len() > 1
        && p.len() <= MAX_PATH
        && p.starts_with('/')
        && !p.ends_with('/')
        && p.bytes().all(|c| c.is_ascii_graphic() && c != b'\\')
        && p.split('/')
            .skip(1)
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.')
}

fn split_word(s: &str) -> (&str, &str) {
    match s.find(' ') {
        Some(i) => (&s[..i], s[i + 1..].trim_start_matches(' ')),
        None => (s, ""),
    }
}

fn file_ref(value: &str) -> Result<(FileRef<'_>, &str), ErrorKind> {
    let (path, rest) = split_word(value);
    if !valid_path(path) {
        return Err(ErrorKind::BadPath);
    }
    let (hash, rest) = split_word(rest);
    let hex = hash.strip_prefix("sha512:").ok_or(ErrorKind::BadHash)?;
    let sha512 = parse_hex::<64>(hex).ok_or(ErrorKind::BadHash)?;
    Ok((FileRef { path, sha512 }, rest))
}

fn on_off(v: &str) -> Result<bool, ErrorKind> {
    match v {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(ErrorKind::BadValue),
    }
}

fn number(v: &str) -> Result<u64, ErrorKind> {
    if v.is_empty() || v.len() > 20 || !v.bytes().all(|c| c.is_ascii_digit()) {
        return Err(ErrorKind::BadValue);
    }
    v.parse().map_err(|_| ErrorKind::BadValue)
}

pub fn parse(text: &[u8]) -> Result<Config<'_>, ConfigError> {
    let err = |line, kind| ConfigError { line, kind };
    if text.len() > MAX_CONFIG_SIZE {
        return Err(err(0, ErrorKind::TooLarge));
    }
    if !text
        .iter()
        .all(|&c| c == b'\n' || c == b'\r' || c == b'\t' || (0x20..0x7f).contains(&c))
    {
        return Err(err(0, ErrorKind::NotText));
    }
    let text = core::str::from_utf8(text).map_err(|_| err(0, ErrorKind::NotText))?;

    let mut cfg = Config {
        timeout: 3,
        default: 0,
        kaslr: true,
        resolution: None,
        svn: 0,
        entries: [None; MAX_ENTRIES],
        entry_count: 0,
    };
    let mut default_id: Option<(&str, usize)> = None;
    let mut seen_header = false;
    let mut seen = [false; 5]; // timeout, default, kaslr, resolution, svn
    let mut entry_seen = [false; 3]; // title, kernel, cmdline

    for (n, raw) in text.split('\n').enumerate() {
        let line_no = n + 1;
        let line = raw.trim_end_matches('\r');
        let line = match line.find('#') {
            Some(i) => &line[..i],
            None => line,
        };
        let line = line.trim_matches(|c| c == ' ' || c == '\t');
        if line.is_empty() {
            continue;
        }
        let line_tabs_free = !line.contains('\t');
        let (key, value) = split_word(line);
        if !seen_header {
            if key != "optimal-boot-config" {
                return Err(err(line_no, ErrorKind::MissingHeader));
            }
            if value != "1" {
                return Err(err(line_no, ErrorKind::UnsupportedVersion));
            }
            seen_header = true;
            continue;
        }
        if !line_tabs_free {
            return Err(err(line_no, ErrorKind::BadValue));
        }
        let in_entry = cfg.entry_count > 0;
        let once = |flag: &mut bool| {
            if *flag {
                Err(err(line_no, ErrorKind::Duplicate))
            } else {
                *flag = true;
                Ok(())
            }
        };
        let need = |v: &str| {
            if v.is_empty() {
                Err(err(line_no, ErrorKind::MissingValue))
            } else {
                Ok(())
            }
        };
        match key {
            "timeout" | "default" | "kaslr" | "resolution" | "svn" if in_entry => {
                return Err(err(line_no, ErrorKind::BadValue));
            }
            "timeout" => {
                once(&mut seen[0])?;
                need(value)?;
                let t = number(value).map_err(|k| err(line_no, k))?;
                if t > MAX_TIMEOUT as u64 {
                    return Err(err(line_no, ErrorKind::BadValue));
                }
                cfg.timeout = t as u32;
            }
            "default" => {
                once(&mut seen[1])?;
                need(value)?;
                if !valid_id(value) {
                    return Err(err(line_no, ErrorKind::BadValue));
                }
                default_id = Some((value, line_no));
            }
            "kaslr" => {
                once(&mut seen[2])?;
                cfg.kaslr = on_off(value).map_err(|k| err(line_no, k))?;
            }
            "resolution" => {
                once(&mut seen[3])?;
                let (w, h) = value
                    .split_once('x')
                    .ok_or(err(line_no, ErrorKind::BadValue))?;
                let w = number(w).map_err(|k| err(line_no, k))?;
                let h = number(h).map_err(|k| err(line_no, k))?;
                if !(1..=16384).contains(&w) || !(1..=16384).contains(&h) {
                    return Err(err(line_no, ErrorKind::BadValue));
                }
                cfg.resolution = Some((w as u32, h as u32));
            }
            "svn" => {
                once(&mut seen[4])?;
                cfg.svn = number(value).map_err(|k| err(line_no, k))?;
            }
            "entry" => {
                if cfg.entry_count > 0 && cfg.entries[cfg.entry_count - 1].unwrap().kernel.is_none()
                {
                    let l = cfg.entries[cfg.entry_count - 1].unwrap().line;
                    return Err(err(l, ErrorKind::MissingKernel));
                }
                if !valid_id(value) {
                    return Err(err(line_no, ErrorKind::BadValue));
                }
                if cfg.entries().any(|e| e.id == value) {
                    return Err(err(line_no, ErrorKind::Duplicate));
                }
                if cfg.entry_count == MAX_ENTRIES {
                    return Err(err(line_no, ErrorKind::TooManyEntries));
                }
                cfg.entries[cfg.entry_count] = Some(Entry {
                    id: value,
                    title: value,
                    kernel: None,
                    cmdline: "",
                    modules: [None; MAX_MODULES],
                    module_count: 0,
                    line: line_no,
                });
                cfg.entry_count += 1;
                entry_seen = [false; 3];
            }
            "title" | "kernel" | "cmdline" | "module" if !in_entry => {
                return Err(err(line_no, ErrorKind::OutsideEntry));
            }
            "title" => {
                once(&mut entry_seen[0])?;
                need(value)?;
                if value.len() > MAX_TITLE {
                    return Err(err(line_no, ErrorKind::BadValue));
                }
                cfg.entries[cfg.entry_count - 1].as_mut().unwrap().title = value;
            }
            "kernel" => {
                once(&mut entry_seen[1])?;
                need(value)?;
                let (f, rest) = file_ref(value).map_err(|k| err(line_no, k))?;
                if !rest.is_empty() {
                    return Err(err(line_no, ErrorKind::BadValue));
                }
                cfg.entries[cfg.entry_count - 1].as_mut().unwrap().kernel = Some(f);
            }
            "cmdline" => {
                once(&mut entry_seen[2])?;
                if value.len() > MAX_CMDLINE {
                    return Err(err(line_no, ErrorKind::BadValue));
                }
                cfg.entries[cfg.entry_count - 1].as_mut().unwrap().cmdline = value;
            }
            "module" => {
                need(value)?;
                let (file, string) = file_ref(value).map_err(|k| err(line_no, k))?;
                if string.len() > MAX_CMDLINE {
                    return Err(err(line_no, ErrorKind::BadValue));
                }
                let e = cfg.entries[cfg.entry_count - 1].as_mut().unwrap();
                if e.module_count == MAX_MODULES {
                    return Err(err(line_no, ErrorKind::TooManyModules));
                }
                e.modules[e.module_count] = Some(Module { file, string });
                e.module_count += 1;
            }
            _ => return Err(err(line_no, ErrorKind::UnknownKey)),
        }
    }
    if !seen_header {
        return Err(err(0, ErrorKind::MissingHeader));
    }
    if cfg.entry_count == 0 {
        return Err(err(0, ErrorKind::NoEntries));
    }
    let last = cfg.entries[cfg.entry_count - 1].unwrap();
    if last.kernel.is_none() {
        return Err(err(last.line, ErrorKind::MissingKernel));
    }
    if let Some((id, line)) = default_id {
        let pos = cfg.entries().position(|e| e.id == id);
        cfg.default = pos.ok_or(err(line, ErrorKind::UnknownDefault))?;
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::format;
    use std::string::String;

    fn h(c: char) -> String {
        core::iter::repeat_n(c, 128).collect()
    }

    #[test]
    fn parses_a_full_config() {
        let text = format!(
            "# comment\noptimal-boot-config 1\ntimeout 5\ndefault second\nkaslr off\nresolution 1920x1080\nsvn 7\n\
             \nentry first\n  title First one\n  kernel /boot/k1 sha512:{}\n\
             entry second\n  kernel /boot/k2 sha512:{}  \n  cmdline  quiet  loglevel=3\n  module /boot/m1 sha512:{} hello world\n  module /boot/m2 sha512:{}\n",
            h('a'),
            h('B'),
            h('0'),
            h('1')
        );
        let c = parse(text.as_bytes()).unwrap();
        assert_eq!(c.timeout, 5);
        assert!(!c.kaslr);
        assert_eq!(c.resolution, Some((1920, 1080)));
        assert_eq!(c.svn, 7);
        assert_eq!(c.entry_count(), 2);
        assert_eq!(c.default, 1);
        let e0 = c.entry(0);
        assert_eq!(e0.title, "First one");
        assert_eq!(e0.kernel().path, "/boot/k1");
        assert_eq!(e0.kernel().sha512, [0xaa; 64]);
        assert_eq!(e0.cmdline, "");
        let e1 = c.entry(1);
        assert_eq!(e1.title, "second");
        assert_eq!(e1.kernel().sha512, [0xbb; 64]);
        assert_eq!(e1.cmdline, "quiet  loglevel=3");
        let mods: std::vec::Vec<_> = e1.modules().collect();
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].string, "hello world");
        assert_eq!(mods[1].string, "");
        assert_eq!(mods[1].file.sha512, [0x11; 64]);
    }

    fn kind(text: &str) -> ErrorKind {
        parse(text.as_bytes()).unwrap_err().kind
    }

    #[test]
    fn rejects_mistakes() {
        let k = format!("kernel /boot/k sha512:{}", h('a'));
        assert_eq!(kind("timeout 3\n"), ErrorKind::MissingHeader);
        assert_eq!(
            kind("optimal-boot-config 2\n"),
            ErrorKind::UnsupportedVersion
        );
        assert_eq!(kind("optimal-boot-config 1\n"), ErrorKind::NoEntries);
        assert_eq!(
            kind("optimal-boot-config 1\nbogus 1\n"),
            ErrorKind::UnknownKey
        );
        assert_eq!(
            kind("optimal-boot-config 1\ntimeout 99\n"),
            ErrorKind::BadValue
        );
        assert_eq!(
            kind("optimal-boot-config 1\ntimeout 1\ntimeout 2\n"),
            ErrorKind::Duplicate
        );
        assert_eq!(
            kind(&format!("optimal-boot-config 1\n{k}\n")),
            ErrorKind::OutsideEntry
        );
        assert_eq!(
            kind("optimal-boot-config 1\nentry a\n"),
            ErrorKind::MissingKernel
        );
        assert_eq!(
            kind("optimal-boot-config 1\nentry a\nentry b\n"),
            ErrorKind::MissingKernel
        );
        assert_eq!(
            kind(&format!(
                "optimal-boot-config 1\nentry a\n{k}\nentry a\n{k}\n"
            )),
            ErrorKind::Duplicate
        );
        assert_eq!(
            kind(&format!(
                "optimal-boot-config 1\ndefault zz\nentry a\n{k}\n"
            )),
            ErrorKind::UnknownDefault
        );
        assert_eq!(
            kind(&format!(
                "optimal-boot-config 1\nentry a\nkernel /boot/../k sha512:{}\n",
                h('a')
            )),
            ErrorKind::BadPath
        );
        assert_eq!(
            kind("optimal-boot-config 1\nentry a\nkernel boot/k sha512:00\n"),
            ErrorKind::BadPath
        );
        assert_eq!(
            kind("optimal-boot-config 1\nentry a\nkernel /boot/k sha512:00\n"),
            ErrorKind::BadHash
        );
        assert_eq!(
            kind(&format!(
                "optimal-boot-config 1\nentry a\nkernel /boot/k {}\n",
                h('a')
            )),
            ErrorKind::BadHash
        );
        assert_eq!(
            kind(&format!("optimal-boot-config 1\nentry a\n{k} extra\n")),
            ErrorKind::BadValue
        );
        assert_eq!(
            kind(&format!("optimal-boot-config 1\nentry a\n{k}\ntimeout 3\n")),
            ErrorKind::BadValue
        );
        assert_eq!(
            kind("optimal-boot-config 1\nentry a\u{1}\n"),
            ErrorKind::NotText
        );
        assert_eq!(
            kind("optimal-boot-config 1\nentry a\u{e9}\n"),
            ErrorKind::NotText
        );
    }

    #[test]
    fn fuzz_never_panics() {
        let base = format!(
            "optimal-boot-config 1\ntimeout 3\nentry a\nkernel /boot/k sha512:{}\ncmdline x y\nmodule /m sha512:{} s\n",
            h('c'),
            h('d')
        );
        let mut rng = crate::testrng::Rng::new(11);
        for _ in 0..50_000 {
            let mut b = base.clone().into_bytes();
            for _ in 0..(1 + rng.below(6)) {
                let at = rng.below(b.len() as u64) as usize;
                match rng.below(3) {
                    0 => b[at] = rng.next() as u8,
                    1 => {
                        b.remove(at);
                    }
                    _ => b.insert(at, b" \n#:/x."[rng.below(7) as usize]),
                }
            }
            let _ = parse(&b);
        }
    }
}
