//! ELF64 kernel loading: validation, layout, copying and relocation.
//!
//! Everything here treats the kernel file as hostile input: every offset and
//! size is bounds-checked with overflow-safe arithmetic, so a corrupt or
//! malicious file is refused with an error instead of crashing the loader.

use crate::protocol::KERNEL_BASE_MIN;

pub const PAGE: u64 = 4096;
pub const MAX_SEGMENTS: usize = 16;
/// Largest kernel image we load (virtual span). The kernel must fit in the
/// top 2 GiB anyway; this keeps a corrupt file from asking for absurd sizes.
pub const MAX_SPAN: u64 = 1 << 30;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_GNU_RELRO: u32 = 0x6474_e552;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;

const DT_NULL: u64 = 0;
const DT_PLTRELSZ: u64 = 2;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_RELAENT: u64 = 9;
const DT_REL: u64 = 17;
const DT_RELSZ: u64 = 18;
const DT_JMPREL: u64 = 23;
const DT_RELRSZ: u64 = 35;
const DT_RELR: u64 = 36;
const DT_RELRENT: u64 = 37;

const R_X86_64_NONE: u32 = 0;
const R_X86_64_RELATIVE: u32 = 8;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ElfError {
    TooSmall,
    BadMagic,
    Not64BitLittleEndian,
    BadVersion,
    NotX86_64,
    BadType(u16),
    BadProgramHeaders,
    TooManySegments,
    NoLoadableSegments,
    SegmentOutsideFile,
    SegmentFileSizeTooBig,
    BadAlignment,
    SegmentsOverlap,
    WritableAndExecutable,
    Interpreter,
    AddressOverflow,
    TooLarge,
    NotInHigherHalf,
    EntryNotExecutable,
    BadDynamic,
    UnsupportedRelocation(u32),
    RelocationOutOfBounds,
    NoSlideFits,
}

impl ElfError {
    pub fn describe(&self) -> &'static str {
        match self {
            ElfError::TooSmall => "file is too small to be an ELF executable",
            ElfError::BadMagic => "not an ELF file",
            ElfError::Not64BitLittleEndian => "not a 64-bit little-endian ELF file",
            ElfError::BadVersion => "unknown ELF version",
            ElfError::NotX86_64 => "not an x86-64 executable",
            ElfError::BadType(_) => "ELF file is neither an executable nor position independent",
            ElfError::BadProgramHeaders => "program headers are missing or corrupt",
            ElfError::TooManySegments => "too many loadable segments",
            ElfError::NoLoadableSegments => "no loadable segments",
            ElfError::SegmentOutsideFile => "a segment points outside the file",
            ElfError::SegmentFileSizeTooBig => "a segment's file size exceeds its memory size",
            ElfError::BadAlignment => "a segment has an invalid alignment",
            ElfError::SegmentsOverlap => "segments with different permissions share a page",
            ElfError::WritableAndExecutable => {
                "a segment is both writable and executable (W^X is enforced)"
            }
            ElfError::Interpreter => "executable asks for a dynamic linker",
            ElfError::AddressOverflow => "segment addresses overflow",
            ElfError::TooLarge => "kernel image is larger than 1 GiB",
            ElfError::NotInHigherHalf => {
                "non-relocatable kernel is not linked at or above 0xffffffff80000000"
            }
            ElfError::EntryNotExecutable => "entry point is not inside an executable segment",
            ElfError::BadDynamic => "dynamic section is corrupt",
            ElfError::UnsupportedRelocation(_) => "kernel uses an unsupported relocation type",
            ElfError::RelocationOutOfBounds => "a relocation points outside the kernel image",
            ElfError::NoSlideFits => "relocatable kernel does not fit in the top 2 GiB",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub memsz: u64,
    pub offset: u64,
    pub filesz: u64,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub pie: bool,
    pub entry: u64,
    /// Lowest loadable address rounded down to a page (link-time address).
    pub base: u64,
    /// Image size in bytes from `base`, a multiple of the page size.
    pub span: u64,
    /// Alignment every slide must keep (largest segment alignment, >= page).
    pub align: u64,
    pub segments: [Segment; MAX_SEGMENTS],
    pub segment_count: usize,
    /// PT_DYNAMIC (link-time address, size) for relocatable kernels.
    pub dynamic: Option<(u64, u64)>,
    /// PT_GNU_RELRO (link-time address, size): read-only after relocation.
    pub relro: Option<(u64, u64)>,
}

impl Layout {
    pub fn segments(&self) -> &[Segment] {
        &self.segments[..self.segment_count]
    }
}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(off..off.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(off..off.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(off..off.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn page_down(x: u64) -> u64 {
    x & !(PAGE - 1)
}
fn page_up(x: u64) -> Option<u64> {
    Some(x.checked_add(PAGE - 1)? & !(PAGE - 1))
}

/// Parses and validates the kernel file.
pub fn parse(file: &[u8]) -> Result<Layout, ElfError> {
    if file.len() < 64 {
        return Err(ElfError::TooSmall);
    }
    if file[0..4] != [0x7f, b'E', b'L', b'F'] {
        return Err(ElfError::BadMagic);
    }
    if file[4] != 2 || file[5] != 1 {
        return Err(ElfError::Not64BitLittleEndian);
    }
    if file[6] != 1 || u32_at(file, 20) != Some(1) {
        return Err(ElfError::BadVersion);
    }
    let e_type = u16_at(file, 16).unwrap();
    let pie = match e_type {
        ET_EXEC => false,
        ET_DYN => true,
        t => return Err(ElfError::BadType(t)),
    };
    if u16_at(file, 18) != Some(EM_X86_64) {
        return Err(ElfError::NotX86_64);
    }
    let entry = u64_at(file, 24).unwrap();
    let phoff = u64_at(file, 32).unwrap();
    let phentsize = u16_at(file, 54).unwrap() as u64;
    let phnum = u16_at(file, 56).unwrap() as u64;
    if phentsize != 56 || phnum == 0 || phnum == 0xffff {
        return Err(ElfError::BadProgramHeaders);
    }
    let ph_end = phnum
        .checked_mul(56)
        .and_then(|n| n.checked_add(phoff))
        .ok_or(ElfError::BadProgramHeaders)?;
    if ph_end > file.len() as u64 {
        return Err(ElfError::BadProgramHeaders);
    }

    let mut layout = Layout {
        pie,
        entry,
        base: 0,
        span: 0,
        align: PAGE,
        segments: [Segment::default(); MAX_SEGMENTS],
        segment_count: 0,
        dynamic: None,
        relro: None,
    };
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    for i in 0..phnum {
        let ph = (phoff + i * 56) as usize;
        let p_type = u32_at(file, ph).unwrap();
        let p_flags = u32_at(file, ph + 4).unwrap();
        let p_offset = u64_at(file, ph + 8).unwrap();
        let p_vaddr = u64_at(file, ph + 16).unwrap();
        let p_filesz = u64_at(file, ph + 32).unwrap();
        let p_memsz = u64_at(file, ph + 40).unwrap();
        let p_align = u64_at(file, ph + 48).unwrap();
        match p_type {
            PT_INTERP => return Err(ElfError::Interpreter),
            PT_DYNAMIC => {
                if pie {
                    layout.dynamic = Some((p_vaddr, p_memsz));
                }
            }
            PT_GNU_RELRO => layout.relro = Some((p_vaddr, p_memsz)),
            PT_LOAD => {
                if p_memsz == 0 {
                    continue;
                }
                if p_filesz > p_memsz {
                    return Err(ElfError::SegmentFileSizeTooBig);
                }
                let file_end = p_offset
                    .checked_add(p_filesz)
                    .ok_or(ElfError::SegmentOutsideFile)?;
                if file_end > file.len() as u64 {
                    return Err(ElfError::SegmentOutsideFile);
                }
                if p_align > 1 && !p_align.is_power_of_two() {
                    return Err(ElfError::BadAlignment);
                }
                if p_align > 1 << 30 {
                    return Err(ElfError::BadAlignment);
                }
                if p_flags & PF_W != 0 && p_flags & PF_X != 0 {
                    return Err(ElfError::WritableAndExecutable);
                }
                let end = p_vaddr
                    .checked_add(p_memsz)
                    .ok_or(ElfError::AddressOverflow)?;
                let end_page = page_up(end).ok_or(ElfError::AddressOverflow)?;
                if layout.segment_count == MAX_SEGMENTS {
                    return Err(ElfError::TooManySegments);
                }
                // Pages may not be shared by segments with different permissions,
                // and segment byte ranges may never overlap.
                let start_page = page_down(p_vaddr);
                for s in layout.segments() {
                    let s_end = s.vaddr + s.memsz;
                    if p_vaddr < s_end && s.vaddr < end {
                        return Err(ElfError::SegmentsOverlap);
                    }
                    let s_start_page = page_down(s.vaddr);
                    let s_end_page = page_up(s_end).unwrap();
                    if start_page < s_end_page && s_start_page < end_page && s.flags != p_flags {
                        return Err(ElfError::SegmentsOverlap);
                    }
                }
                layout.segments[layout.segment_count] = Segment {
                    vaddr: p_vaddr,
                    memsz: p_memsz,
                    offset: p_offset,
                    filesz: p_filesz,
                    flags: p_flags,
                };
                layout.segment_count += 1;
                lo = lo.min(start_page);
                hi = hi.max(end_page);
                layout.align = layout.align.max(p_align);
            }
            _ => {}
        }
    }
    if layout.segment_count == 0 {
        return Err(ElfError::NoLoadableSegments);
    }
    // Relocations are only reachable through PT_DYNAMIC; without it the
    // image can only run where it was linked.
    if layout.pie && layout.dynamic.is_none() {
        layout.pie = false;
    }
    layout.base = lo;
    layout.span = hi - lo;
    if layout.span > MAX_SPAN {
        return Err(ElfError::TooLarge);
    }
    if !layout.pie && lo < KERNEL_BASE_MIN {
        return Err(ElfError::NotInHigherHalf);
    }
    if !layout
        .segments()
        .iter()
        .any(|s| s.flags & PF_X != 0 && entry >= s.vaddr && entry < s.vaddr + s.memsz)
    {
        return Err(ElfError::EntryNotExecutable);
    }
    if let Some((addr, size)) = layout.dynamic
        && (addr < lo || addr.checked_add(size).is_none_or(|e| e > hi) || size % 16 != 0)
    {
        return Err(ElfError::BadDynamic);
    }
    Ok(layout)
}

/// The range of slides (multiples of `layout.align`) that place a
/// relocatable image entirely in the top 2 GiB: `(min, count)`, where slide
/// `min + k * align` is valid for every `k < count`. Non-relocatable images
/// have exactly one slide, 0.
pub fn slide_range(layout: &Layout) -> Result<(u64, u64), ElfError> {
    if !layout.pie {
        return Ok((0, 1));
    }
    let align = layout.align as u128;
    let base = layout.base as u128;
    let end_limit: u128 = 1u128 << 64;
    let min_base = KERNEL_BASE_MIN as u128;
    // slide s (mod 2^64 arithmetic is avoided by working in u128 and requiring
    // base + s >= min_base and base + s + span <= 2^64).
    let lo = min_base.saturating_sub(base);
    let lo = lo.div_ceil(align) * align;
    let hi = end_limit - layout.span as u128 - base; // max slide
    let hi = hi / align * align;
    if lo > hi {
        return Err(ElfError::NoSlideFits);
    }
    let count = (hi - lo) / align + 1;
    Ok((lo as u64, count.min(u64::MAX as u128) as u64))
}

/// Copies every segment into `image` (the kernel's memory, `layout.span`
/// bytes for the range starting at `layout.base`) and zeroes the rest.
pub fn load(file: &[u8], layout: &Layout, image: &mut [u8]) {
    assert_eq!(image.len() as u64, layout.span);
    image.fill(0);
    for s in layout.segments() {
        let at = (s.vaddr - layout.base) as usize;
        let src = &file[s.offset as usize..(s.offset + s.filesz) as usize];
        image[at..at + src.len()].copy_from_slice(src);
    }
}

fn image_offset(layout: &Layout, vaddr: u64, len: u64) -> Result<usize, ElfError> {
    let off = vaddr
        .checked_sub(layout.base)
        .ok_or(ElfError::RelocationOutOfBounds)?;
    if off.checked_add(len).is_none_or(|e| e > layout.span) {
        return Err(ElfError::RelocationOutOfBounds);
    }
    Ok(off as usize)
}

/// Applies the kernel's relocations for `slide`. Only R_X86_64_RELATIVE
/// (as RELA or RELR) is accepted; anything that needs symbols is refused.
/// The image must already be loaded. Returns the number of relocations done.
pub fn relocate(layout: &Layout, image: &mut [u8], slide: u64) -> Result<usize, ElfError> {
    let Some((dyn_addr, dyn_size)) = layout.dynamic else {
        return Ok(0);
    };
    let dyn_off = image_offset(layout, dyn_addr, dyn_size)?;
    let mut rela = None;
    let mut relasz = None;
    let mut relaent = 24;
    let mut relr = None;
    let mut relrsz = None;
    let mut relrent = 8;
    let mut i = 0;
    while i + 16 <= dyn_size as usize {
        let tag = u64_at(image, dyn_off + i).unwrap();
        let val = u64_at(image, dyn_off + i + 8).unwrap();
        match tag {
            DT_NULL => break,
            DT_RELA => rela = Some(val),
            DT_RELASZ => relasz = Some(val),
            DT_RELAENT => relaent = val,
            DT_RELR => relr = Some(val),
            DT_RELRSZ => relrsz = Some(val),
            DT_RELRENT => relrent = val,
            DT_REL | DT_RELSZ => return Err(ElfError::UnsupportedRelocation(0xffff_0000)),
            DT_JMPREL | DT_PLTRELSZ if val != 0 => {
                return Err(ElfError::UnsupportedRelocation(0xffff_0001));
            }
            _ => {}
        }
        i += 16;
    }
    let mut done = 0;
    let mut apply = |image: &mut [u8], at_vaddr: u64, value: Option<u64>| {
        let off = image_offset(layout, at_vaddr, 8)?;
        let old = u64_at(image, off).unwrap();
        let new = match value {
            Some(v) => v,
            None => old.wrapping_add(slide),
        };
        image[off..off + 8].copy_from_slice(&new.to_le_bytes());
        done += 1;
        Ok::<(), ElfError>(())
    };
    match (rela, relasz) {
        (Some(addr), Some(size)) => {
            if relaent != 24 || size % 24 != 0 {
                return Err(ElfError::BadDynamic);
            }
            let off = image_offset(layout, addr, size)?;
            for k in 0..(size / 24) as usize {
                let e = off + k * 24;
                let r_offset = u64_at(image, e).unwrap();
                let r_info = u64_at(image, e + 8).unwrap();
                let r_addend = u64_at(image, e + 16).unwrap();
                match r_info as u32 {
                    R_X86_64_NONE => {}
                    R_X86_64_RELATIVE => {
                        apply(image, r_offset, Some(r_addend.wrapping_add(slide)))?
                    }
                    t => return Err(ElfError::UnsupportedRelocation(t)),
                }
            }
        }
        (None, None) => {}
        _ => return Err(ElfError::BadDynamic),
    }
    match (relr, relrsz) {
        (Some(addr), Some(size)) => {
            if relrent != 8 || size % 8 != 0 {
                return Err(ElfError::BadDynamic);
            }
            let off = image_offset(layout, addr, size)?;
            let mut where_ = 0u64;
            for k in 0..(size / 8) as usize {
                let entry = u64_at(image, off + k * 8).unwrap();
                if entry & 1 == 0 {
                    apply(image, entry, None)?;
                    where_ = entry
                        .checked_add(8)
                        .ok_or(ElfError::RelocationOutOfBounds)?;
                } else {
                    let mut bits = entry >> 1;
                    let mut j = 0u64;
                    while bits != 0 {
                        if bits & 1 != 0 {
                            apply(image, where_ + j * 8, None)?;
                        }
                        bits >>= 1;
                        j += 1;
                    }
                    where_ = where_
                        .checked_add(63 * 8)
                        .ok_or(ElfError::RelocationOutOfBounds)?;
                }
            }
        }
        (None, None) => {}
        _ => return Err(ElfError::BadDynamic),
    }
    Ok(done)
}

/// Page permissions for the kernel mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagePerms {
    pub write: bool,
    pub exec: bool,
}

/// Permissions of the page at link-time address `page` (page aligned), or
/// None if no segment touches it (gaps are left unmapped). RELRO pages that
/// lie wholly inside PT_GNU_RELRO become read-only once relocated.
pub fn page_perms(layout: &Layout, page: u64) -> Option<PagePerms> {
    let seg = layout.segments().iter().find(|s| {
        let start = page_down(s.vaddr);
        let end = page_up(s.vaddr + s.memsz).unwrap();
        page >= start && page < end
    })?;
    let mut write = seg.flags & PF_W != 0;
    if let Some((addr, size)) = layout.relro
        && let (Some(start), Some(end)) = (page_up(addr), addr.checked_add(size).map(page_down))
        && page >= start
        && page + PAGE <= end
    {
        write = false;
    }
    Some(PagePerms {
        write,
        exec: seg.flags & PF_X != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    struct Ph {
        typ: u32,
        flags: u32,
        offset: u64,
        vaddr: u64,
        filesz: u64,
        memsz: u64,
        align: u64,
    }

    fn build(e_type: u16, entry: u64, phs: &[Ph], body_len: usize) -> Vec<u8> {
        let mut f = vec![0u8; 64 + phs.len() * 56];
        f[0..4].copy_from_slice(b"\x7fELF");
        f[4] = 2;
        f[5] = 1;
        f[6] = 1;
        f[16..18].copy_from_slice(&e_type.to_le_bytes());
        f[18..20].copy_from_slice(&62u16.to_le_bytes());
        f[20..24].copy_from_slice(&1u32.to_le_bytes());
        f[24..32].copy_from_slice(&entry.to_le_bytes());
        f[32..40].copy_from_slice(&64u64.to_le_bytes());
        f[54..56].copy_from_slice(&56u16.to_le_bytes());
        f[56..58].copy_from_slice(&(phs.len() as u16).to_le_bytes());
        for (i, p) in phs.iter().enumerate() {
            let o = 64 + i * 56;
            f[o..o + 4].copy_from_slice(&p.typ.to_le_bytes());
            f[o + 4..o + 8].copy_from_slice(&p.flags.to_le_bytes());
            f[o + 8..o + 16].copy_from_slice(&p.offset.to_le_bytes());
            f[o + 16..o + 24].copy_from_slice(&p.vaddr.to_le_bytes());
            f[o + 32..o + 40].copy_from_slice(&p.filesz.to_le_bytes());
            f[o + 40..o + 48].copy_from_slice(&p.memsz.to_le_bytes());
            f[o + 48..o + 56].copy_from_slice(&p.align.to_le_bytes());
        }
        f.resize(f.len().max(body_len), 0xAA);
        f
    }

    fn load_ph(flags: u32, offset: u64, vaddr: u64, filesz: u64, memsz: u64) -> Ph {
        Ph {
            typ: PT_LOAD,
            flags,
            offset,
            vaddr,
            filesz,
            memsz,
            align: 0x1000,
        }
    }

    const K: u64 = KERNEL_BASE_MIN;

    #[test]
    fn typical_kernel() {
        let f = build(
            ET_EXEC,
            K + 0x10,
            &[
                load_ph(PF_R | PF_X, 0x1000, K, 0x1800, 0x1800),
                load_ph(PF_R, 0x3000, K + 0x2000, 0x100, 0x100),
                load_ph(PF_R | PF_W, 0x4000, K + 0x3000, 0x80, 0x5000),
            ],
            0x5000,
        );
        let l = parse(&f).unwrap();
        assert!(!l.pie);
        assert_eq!(l.base, K);
        assert_eq!(l.span, 0x8000);
        assert_eq!(slide_range(&l).unwrap(), (0, 1));
        let mut img = vec![0xFFu8; l.span as usize];
        load(&f, &l, &mut img);
        assert_eq!(img[0], 0xAA);
        assert_eq!(img[0x17ff], 0xAA);
        assert_eq!(img[0x1800], 0);
        assert_eq!(img[0x3000 + 0x7f], 0xAA);
        assert_eq!(img[0x3000 + 0x80], 0); // bss zeroed
        assert_eq!(
            page_perms(&l, K),
            Some(PagePerms {
                write: false,
                exec: true
            })
        );
        assert_eq!(
            page_perms(&l, K + 0x2000),
            Some(PagePerms {
                write: false,
                exec: false
            })
        );
        assert_eq!(
            page_perms(&l, K + 0x7000),
            Some(PagePerms {
                write: true,
                exec: false
            })
        );
    }

    #[test]
    fn refuses_bad_kernels() {
        let ok = |phs: &[Ph], entry| parse(&build(ET_EXEC, entry, phs, 0x4000));
        assert_eq!(
            ok(&[load_ph(PF_R | PF_W | PF_X, 0x1000, K, 0x100, 0x100)], K).unwrap_err(),
            ElfError::WritableAndExecutable
        );
        assert_eq!(
            ok(
                &[load_ph(PF_R | PF_X, 0x1000, 0x400000, 0x100, 0x100)],
                0x400000
            )
            .unwrap_err(),
            ElfError::NotInHigherHalf
        );
        assert_eq!(
            ok(&[load_ph(PF_R | PF_X, 0x1000, K, 0x100, 0x100)], K + 0x200).unwrap_err(),
            ElfError::EntryNotExecutable
        );
        assert_eq!(
            ok(&[load_ph(PF_R | PF_X, 0x3000, K, 0x2000, 0x2000)], K).unwrap_err(),
            ElfError::SegmentOutsideFile
        );
        assert_eq!(
            ok(&[load_ph(PF_R | PF_X, 0x1000, K, 0x200, 0x100)], K).unwrap_err(),
            ElfError::SegmentFileSizeTooBig
        );
        assert_eq!(
            ok(
                &[
                    load_ph(PF_R | PF_X, 0x1000, K, 0x100, 0x100),
                    load_ph(PF_R | PF_W, 0x1000, K + 0x800, 0x100, 0x100)
                ],
                K
            )
            .unwrap_err(),
            ElfError::SegmentsOverlap
        );
        assert_eq!(
            ok(
                &[load_ph(PF_R | PF_X, 0x1000, u64::MAX - 10, 0x10, 0x100)],
                K
            )
            .unwrap_err(),
            ElfError::AddressOverflow
        );
        let mut f = build(ET_EXEC, K, &[load_ph(PF_R | PF_X, 0, K, 0x10, 0x10)], 0x100);
        f[18] = 3;
        assert_eq!(parse(&f).unwrap_err(), ElfError::NotX86_64);
        assert_eq!(parse(&f[..40]).unwrap_err(), ElfError::TooSmall);
    }

    #[test]
    fn dyn_without_dynamic_segment_is_not_slid() {
        // An ET_DYN file with no PT_DYNAMIC (what static-pie linking with a
        // custom PHDRS list produces) has relocations we cannot find, so
        // sliding it would leave every stored pointer wrong. It must load
        // exactly at its link address, which then has to be in the top 2 GiB.
        let f = build(
            ET_DYN,
            K,
            &[load_ph(PF_R | PF_X, 0x1000, K, 0x10, 0x10)],
            0x2000,
        );
        let l = parse(&f).unwrap();
        assert!(!l.pie);
        assert_eq!(slide_range(&l).unwrap(), (0, 1));
        let low = build(
            ET_DYN,
            0x1000,
            &[load_ph(PF_R | PF_X, 0x1000, 0x1000, 0x10, 0x10)],
            0x2000,
        );
        assert_eq!(parse(&low).unwrap_err(), ElfError::NotInHigherHalf);
    }

    #[test]
    fn pie_slides_and_relocates() {
        // Lower-half PIE kernel with one RELA and one RELR block.
        let dyn_vaddr = 0x3000u64;
        let mut f = build(
            ET_DYN,
            0x1000,
            &[
                load_ph(PF_R | PF_X, 0x1000, 0x1000, 0x1000, 0x1000),
                load_ph(PF_R | PF_W, 0x2000, 0x2000, 0x2000, 0x2000),
                Ph {
                    typ: PT_DYNAMIC,
                    flags: PF_R | PF_W,
                    offset: 0x3000,
                    vaddr: dyn_vaddr,
                    filesz: 0x60,
                    memsz: 0x60,
                    align: 8,
                },
            ],
            0x4000,
        );
        let w =
            |f: &mut Vec<u8>, off: usize, v: u64| f[off..off + 8].copy_from_slice(&v.to_le_bytes());
        // Dynamic: RELA at 0x3100 (2 entries), RELR at 0x3200 (2 words).
        let d = 0x3000;
        w(&mut f, d, DT_RELA);
        w(&mut f, d + 8, 0x3100);
        w(&mut f, d + 16, DT_RELASZ);
        w(&mut f, d + 24, 48);
        w(&mut f, d + 32, DT_RELR);
        w(&mut f, d + 40, 0x3200);
        w(&mut f, d + 48, DT_RELRSZ);
        w(&mut f, d + 56, 16);
        w(&mut f, d + 64, DT_NULL);
        // RELA: *(0x2000) = slide + 0x1234 ; NONE
        w(&mut f, 0x3100, 0x2000);
        w(&mut f, 0x3108, R_X86_64_RELATIVE as u64);
        w(&mut f, 0x3110, 0x1234);
        w(&mut f, 0x3118, 0);
        w(&mut f, 0x3120, 0);
        w(&mut f, 0x3128, 0);
        // RELR: address 0x2008, then bitmap covering 0x2010 and 0x2020.
        w(&mut f, 0x3200, 0x2008);
        w(&mut f, 0x3208, 1 | (1 << 1) | (1 << 3));
        w(&mut f, 0x2008, 0x10);
        w(&mut f, 0x2010, 0x20);
        w(&mut f, 0x2018, 0);
        w(&mut f, 0x2020, 0x30);
        let l = parse(&f).unwrap();
        assert!(l.pie);
        let (min, count) = slide_range(&l).unwrap();
        assert_eq!(min + l.base, K);
        assert!(count > 1000);
        let slide = min + 7 * l.align;
        let mut img = vec![0u8; l.span as usize];
        load(&f, &l, &mut img);
        assert_eq!(relocate(&l, &mut img, slide).unwrap(), 4);
        let r = |img: &[u8], va: u64| {
            u64::from_le_bytes(img[(va - l.base) as usize..][..8].try_into().unwrap())
        };
        assert_eq!(r(&img, 0x2000), slide + 0x1234);
        assert_eq!(r(&img, 0x2008), slide + 0x10);
        assert_eq!(r(&img, 0x2010), slide + 0x20);
        assert_eq!(r(&img, 0x2018), 0);
        assert_eq!(r(&img, 0x2020), slide + 0x30);
        // The largest slide keeps the image below 2^64.
        let max_slide = min + (count - 1) * l.align;
        assert!((l.base as u128 + max_slide as u128 + l.span as u128) <= 1u128 << 64);
    }

    #[test]
    fn relocation_out_of_bounds_is_refused() {
        let mut f = build(
            ET_DYN,
            0x1000,
            &[
                load_ph(PF_R | PF_X, 0x1000, 0x1000, 0x1000, 0x1000),
                load_ph(PF_R | PF_W, 0x2000, 0x2000, 0x1000, 0x1000),
                Ph {
                    typ: PT_DYNAMIC,
                    flags: PF_R,
                    offset: 0x2000,
                    vaddr: 0x2000,
                    filesz: 0x40,
                    memsz: 0x40,
                    align: 8,
                },
            ],
            0x3000,
        );
        let w =
            |f: &mut Vec<u8>, off: usize, v: u64| f[off..off + 8].copy_from_slice(&v.to_le_bytes());
        w(&mut f, 0x2000, DT_RELA);
        w(&mut f, 0x2008, 0x2100);
        w(&mut f, 0x2010, DT_RELASZ);
        w(&mut f, 0x2018, 24);
        w(&mut f, 0x2100, 0x9000); // outside the image
        w(&mut f, 0x2108, R_X86_64_RELATIVE as u64);
        let l = parse(&f).unwrap();
        let mut img = vec![0u8; l.span as usize];
        load(&f, &l, &mut img);
        assert_eq!(
            relocate(&l, &mut img, K).unwrap_err(),
            ElfError::RelocationOutOfBounds
        );
        // A symbol-based relocation is refused.
        w(&mut f, 0x2100, 0x2200);
        w(&mut f, 0x2108, 1 | (5 << 32));
        let mut img = vec![0u8; l.span as usize];
        load(&f, &l, &mut img);
        assert_eq!(
            relocate(&l, &mut img, K).unwrap_err(),
            ElfError::UnsupportedRelocation(1)
        );
    }

    #[test]
    fn fuzz_parse_and_relocate_never_panic() {
        let mut rng = crate::testrng::Rng::new(7);
        let seed = build(
            ET_DYN,
            0x1000,
            &[
                load_ph(PF_R | PF_X, 0x1000, 0x1000, 0x1000, 0x1000),
                load_ph(PF_R | PF_W, 0x2000, 0x2000, 0x1000, 0x3000),
                Ph {
                    typ: PT_DYNAMIC,
                    flags: PF_R,
                    offset: 0x2000,
                    vaddr: 0x2000,
                    filesz: 0x40,
                    memsz: 0x40,
                    align: 8,
                },
            ],
            0x3000,
        );
        for _ in 0..30_000 {
            let mut f = seed.clone();
            for _ in 0..(1 + rng.below(8)) {
                let at = rng.below(64 + 3 * 56 + 0x80) as usize;
                let at = if at >= 64 + 3 * 56 {
                    0x2000 + at - (64 + 3 * 56)
                } else {
                    at
                };
                f[at] = rng.next() as u8;
            }
            if let Ok(l) = parse(&f)
                && let Ok((min, count)) = slide_range(&l)
            {
                let mut img = vec![0u8; l.span as usize];
                load(&f, &l, &mut img);
                let slide = min + rng.below(count.min(1 << 20)) * l.align;
                let _ = relocate(&l, &mut img, slide);
            }
        }
    }
}
