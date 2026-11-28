//! Reading files from the volume the loader itself was started from, and
//! working out which disk and partition that is (for the kernel's
//! `limine_file` records).

use crate::console::boot_services;
use crate::efi::{self, Guid};
use crate::memory::Pages;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;
use optimal_boot_core::devpath;

pub struct Volume {
    root: *mut efi::FileProtocol,
    pub info: VolumeInfo,
}

#[derive(Clone, Copy, Default)]
pub struct VolumeInfo {
    pub optical: bool,
    pub partition_index: u32,
    pub mbr_disk_id: u32,
    pub gpt_part_uuid: [u8; 16],
    pub gpt_disk_uuid: [u8; 16],
}

#[derive(Debug)]
pub enum FsError {
    NoVolume,
    NotFound,
    TooLarge,
    ReadFailed,
    OutOfMemory,
}

impl FsError {
    pub fn describe(&self) -> &'static str {
        match self {
            FsError::NoVolume => "cannot open the boot volume",
            FsError::NotFound => "file not found",
            FsError::TooLarge => "file is too large",
            FsError::ReadFailed => "reading the file failed",
            FsError::OutOfMemory => "out of memory loading the file",
        }
    }
}

fn protocol<T>(handle: efi::Handle, guid: &Guid) -> Option<*mut T> {
    let mut p: *mut c_void = ptr::null_mut();
    let s = (boot_services().handle_protocol)(handle, guid, &mut p);
    (s == efi::SUCCESS && !p.is_null()).then_some(p as *mut T)
}

impl Volume {
    pub fn open(image: efi::Handle) -> Result<Volume, FsError> {
        let li: *mut efi::LoadedImage =
            protocol(image, &efi::LOADED_IMAGE_GUID).ok_or(FsError::NoVolume)?;
        // SAFETY: the firmware's loaded image protocol for our own image.
        let device = unsafe { (*li).device_handle };
        let sfs: *mut efi::SimpleFileSystem =
            protocol(device, &efi::SIMPLE_FILE_SYSTEM_GUID).ok_or(FsError::NoVolume)?;
        let mut root = ptr::null_mut();
        // SAFETY: sfs is a valid protocol pointer from the firmware.
        if unsafe { ((*sfs).open_volume)(sfs, &mut root) } != efi::SUCCESS {
            return Err(FsError::NoVolume);
        }
        Ok(Volume {
            root,
            info: volume_info(device),
        })
    }

    /// Reads a whole file (path with '/' separators) into fresh pages, which
    /// keeps it 4 KiB aligned with pages of its own. Returns (pages, size).
    pub fn read(&self, path: &str, max: u64) -> Result<(Pages, u64), FsError> {
        let mut name: Vec<u16> = path
            .bytes()
            .map(|b| if b == b'/' { b'\\' as u16 } else { b as u16 })
            .collect();
        name.push(0);
        let mut file = ptr::null_mut();
        // SAFETY: root is the open volume; name is NUL-terminated UCS-2.
        let s = unsafe {
            ((*self.root).open)(self.root, &mut file, name.as_ptr(), efi::FILE_MODE_READ, 0)
        };
        if s != efi::SUCCESS {
            return Err(FsError::NotFound);
        }
        let result = read_open(file, max);
        // SAFETY: file was opened above.
        unsafe { ((*file).close)(file) };
        result
    }
}

fn read_open(file: *mut efi::FileProtocol, max: u64) -> Result<(Pages, u64), FsError> {
    let mut info = [0u64; 128]; // 1 KiB, 8-byte aligned, room for the name
    let mut size = core::mem::size_of_val(&info);
    // SAFETY: file is open; info is a writable buffer of `size` bytes.
    let s = unsafe {
        ((*file).get_info)(
            file,
            &efi::FILE_INFO_GUID,
            &mut size,
            info.as_mut_ptr() as *mut u8,
        )
    };
    if s != efi::SUCCESS {
        return Err(FsError::ReadFailed);
    }
    // EFI_FILE_INFO: Size, FileSize, PhysicalSize, 3 x EFI_TIME, Attribute.
    let file_size = info[1];
    let attribute = info[8];
    if attribute & efi::FILE_DIRECTORY != 0 {
        return Err(FsError::NotFound);
    }
    if file_size > max {
        return Err(FsError::TooLarge);
    }
    let mut pages = Pages::new(file_size, efi::LOADER_DATA, None).ok_or(FsError::OutOfMemory)?;
    let buf = pages.bytes();
    let mut done = 0u64;
    while done < file_size {
        let mut chunk = ((file_size - done) as usize).min(4 << 20);
        // SAFETY: buf has at least file_size bytes; chunk stays inside it.
        let s = unsafe { ((*file).read)(file, &mut chunk, buf.as_mut_ptr().add(done as usize)) };
        if s != efi::SUCCESS || chunk == 0 {
            pages.free();
            return Err(FsError::ReadFailed);
        }
        done += chunk as u64;
    }
    // A file that grew while being read is refused rather than truncated.
    let mut extra = [0u8; 1];
    let mut n = 1usize;
    // SAFETY: as above.
    let s = unsafe { ((*file).read)(file, &mut n, extra.as_mut_ptr()) };
    if s != efi::SUCCESS || n != 0 {
        pages.free();
        return Err(FsError::ReadFailed);
    }
    Ok((pages, file_size))
}

fn volume_info(device: efi::Handle) -> VolumeInfo {
    let mut v = VolumeInfo::default();
    let Some(dp) = protocol::<efi::DevicePath>(device, &efi::DEVICE_PATH_GUID) else {
        return v;
    };
    let Some(path) = device_path_bytes(dp) else {
        return v;
    };
    // The path was measured, so `volume` cannot fail on it.
    let Ok(vol) = devpath::volume(path) else {
        return v;
    };
    v.optical = vol.optical;
    v.partition_index = vol.partition_index;
    v.mbr_disk_id = vol.mbr_disk_id;
    if let Some(uuid) = vol.gpt_part_uuid {
        v.gpt_part_uuid = uuid;
        v.gpt_disk_uuid = gpt_disk_uuid(&path[..vol.disk_prefix]).unwrap_or_default();
    }
    v
}

/// The bytes of a firmware device path's first instance, End node included,
/// measured node header by node header (core::devpath::measure) up to
/// devpath::MAX_LEN. None for a malformed path.
fn device_path_bytes<'a>(dp: *const efi::DevicePath) -> Option<&'a [u8]> {
    let base = dp as *const u8;
    let len = devpath::measure(
        // SAFETY: `measure` asks only for the header at a node boundary it
        // reached by walking checked node lengths; a firmware device path
        // is readable through its End node.
        |off| Some(unsafe { core::ptr::read_unaligned(base.add(off) as *const [u8; 4]) }),
        devpath::MAX_LEN,
    )
    .ok()?;
    // SAFETY: the measured nodes, End node included, are readable.
    Some(unsafe { core::slice::from_raw_parts(base, len) })
}

fn gpt_disk_uuid(prefix: &[u8]) -> Option<[u8; 16]> {
    let mut path = prefix.to_vec();
    path.extend_from_slice(&[0x7f, 0xff, 4, 0]);
    let mut dp = path.as_ptr() as *const efi::DevicePath;
    let mut handle: efi::Handle = ptr::null_mut();
    if (boot_services().locate_device_path)(&efi::BLOCK_IO_GUID, &mut dp, &mut handle)
        != efi::SUCCESS
    {
        return None;
    }
    let bio: *mut efi::BlockIo = protocol(handle, &efi::BLOCK_IO_GUID)?;
    // SAFETY: bio is the firmware's block I/O for the whole disk.
    let media = unsafe { &*(*bio).media };
    if media.logical_partition || !(512..=4096).contains(&media.block_size) {
        return None;
    }
    let mut buf = Pages::new(media.block_size as u64, efi::LOADER_DATA, None)?;
    // SAFETY: buf has block_size bytes, page aligned (meets any io_align).
    let s = unsafe {
        ((*bio).read_blocks)(
            bio,
            media.media_id,
            1,
            media.block_size as usize,
            buf.bytes().as_mut_ptr(),
        )
    };
    let out = (s == efi::SUCCESS && &buf.bytes()[0..8] == b"EFI PART")
        .then(|| buf.bytes()[56..72].try_into().unwrap());
    buf.free();
    out
}
