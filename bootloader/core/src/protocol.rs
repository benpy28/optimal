//! Limine boot protocol definitions, written from the public specification
//! (limine-protocol PROTOCOL.md, 0BSD). Only the x86-64 parts are defined.
//!
//! The kernel finds everything through "requests": 8-byte aligned structures
//! in its image that start with a 4 x u64 ID. We scan for them, then point
//! each request's `response` field at a response we built.

/// First two words of every request ID.
pub const COMMON_MAGIC: [u64; 2] = [0xc7b1dd30df4c8b88, 0x0a82e883a194f07b];

pub const REQUESTS_START_MARKER: [u64; 4] = [
    0xf6b8f4b39de7d1ae,
    0xfab91a6940fcb9cf,
    0x785c6ed015d3e316,
    0x181e920a7852b9d9,
];
pub const REQUESTS_END_MARKER: [u64; 2] = [0xadc0e0531bb10d03, 0x9572709f31764c62];

/// First two words of a base revision tag; the third word is the revision.
pub const BASE_REVISION_MAGIC: [u64; 2] = [0xf9562b2d5c95a6c8, 0x6a7b384944536bdc];

/// Base revisions this loader implements. 0 to 2 are deprecated by the spec
/// and refused, so a kernel never gets weaker guarantees than it asked for.
pub const MIN_BASE_REVISION: u64 = 3;
pub const MAX_BASE_REVISION: u64 = 6;

/// The third and fourth words of each request ID we know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    BootloaderInfo,
    ExecutableCmdline,
    FirmwareType,
    StackSize,
    Hhdm,
    Framebuffer,
    PagingMode,
    Mp,
    RiscvBspHartid,
    MemoryMap,
    EntryPoint,
    ExecutableFile,
    Module,
    Rsdp,
    Smbios,
    EfiSystemTable,
    TpmEventLog,
    EfiMemoryMap,
    DateAtBoot,
    ExecutableAddress,
    Dtb,
    BootloaderPerformance,
    KeepIommu,
    TscFrequency,
    FlantermFbInitParams,
    Entropy,
}

pub const FEATURES: &[(Feature, [u64; 2])] = &[
    (
        Feature::BootloaderInfo,
        [0xf55038d8e2a1202f, 0x279426fcf5f59740],
    ),
    (
        Feature::ExecutableCmdline,
        [0x4b161536e598651e, 0xb390ad4a2f1f303a],
    ),
    (
        Feature::FirmwareType,
        [0x8c2f75d90bef28a8, 0x7045a4688eac00c3],
    ),
    (Feature::StackSize, [0x224ef0460a8e8926, 0xe1cb0fc25f46ea3d]),
    (Feature::Hhdm, [0x48dcf1cb8ad2b852, 0x63984e959a98244b]),
    (
        Feature::Framebuffer,
        [0x9d5827dcd881dd75, 0xa3148604f6fab11b],
    ),
    (
        Feature::PagingMode,
        [0x95c1a0edab0944cb, 0xa4e5cb3842f7488a],
    ),
    (Feature::Mp, [0x95a67b819a1b857e, 0xa0b61b723b6a73e0]),
    (
        Feature::RiscvBspHartid,
        [0x1369359f025525f9, 0x2ff2a56178391bb6],
    ),
    (Feature::MemoryMap, [0x67cf3d9d378a806f, 0xe304acdfc50c3c62]),
    (
        Feature::EntryPoint,
        [0x13d86c035a1cd3e1, 0x2b0caa89d8f3026a],
    ),
    (
        Feature::ExecutableFile,
        [0xad97e90e83f1ed67, 0x31eb5d1c5ff23b69],
    ),
    (Feature::Module, [0x3e7e279702be32af, 0xca1c4f3bd1280cee]),
    (Feature::Rsdp, [0xc5e77b6b397e7b43, 0x27637845accdcf3c]),
    (Feature::Smbios, [0x9e9046f11e095391, 0xaa4a520fefbde5ee]),
    (
        Feature::EfiSystemTable,
        [0x5ceba5163eaaf6d6, 0x0a6981610cf65fcc],
    ),
    (
        Feature::TpmEventLog,
        [0x98e094fc7e76e979, 0xee8d8775c54e1d1f],
    ),
    (
        Feature::EfiMemoryMap,
        [0x7df62a431d6872d5, 0xa4fcdfb3e57306c8],
    ),
    (
        Feature::DateAtBoot,
        [0x502746e184c088aa, 0xfbc5ec83e6327893],
    ),
    (
        Feature::ExecutableAddress,
        [0x71ba76863cc55f63, 0xb2644a48c516a487],
    ),
    (Feature::Dtb, [0xb40ddb48fb54bac7, 0x545081493f81ffb7]),
    (
        Feature::BootloaderPerformance,
        [0x6b50ad9bf36d13ad, 0xdc4c7e88fc759e17],
    ),
    (Feature::KeepIommu, [0x8ebaabe51f490179, 0x2aa86a59ffb4ab0f]),
    (
        Feature::TscFrequency,
        [0x10f2ee1d87d195e4, 0xf747a2b78f6ddb31],
    ),
    (
        Feature::FlantermFbInitParams,
        [0x3259399fe7c5f126, 0xe01c1c8c5db9d1a9],
    ),
    (Feature::Entropy, [0x65ea80255d5682c5, 0x9117240723f493eb]),
];

pub fn feature_for_id(id: [u64; 2]) -> Option<Feature> {
    FEATURES.iter().find(|(_, i)| *i == id).map(|(f, _)| *f)
}

pub fn id_for_feature(feature: Feature) -> [u64; 4] {
    let (_, id) = FEATURES.iter().find(|(f, _)| *f == feature).unwrap();
    [COMMON_MAGIC[0], COMMON_MAGIC[1], id[0], id[1]]
}

// Memory map entry types.
pub const MEMMAP_USABLE: u64 = 0;
pub const MEMMAP_RESERVED: u64 = 1;
pub const MEMMAP_ACPI_RECLAIMABLE: u64 = 2;
pub const MEMMAP_ACPI_NVS: u64 = 3;
pub const MEMMAP_BAD_MEMORY: u64 = 4;
pub const MEMMAP_BOOTLOADER_RECLAIMABLE: u64 = 5;
pub const MEMMAP_EXECUTABLE_AND_MODULES: u64 = 6;
pub const MEMMAP_FRAMEBUFFER: u64 = 7;
pub const MEMMAP_RESERVED_MAPPED: u64 = 8;

pub const FIRMWARE_TYPE_X86BIOS: u64 = 0;
pub const FIRMWARE_TYPE_EFI64: u64 = 2;

pub const PAGING_MODE_4LVL: u64 = 0;
pub const PAGING_MODE_5LVL: u64 = 1;

pub const FRAMEBUFFER_RGB: u8 = 1;

pub const MEDIA_TYPE_GENERIC: u32 = 0;
pub const MEDIA_TYPE_OPTICAL: u32 = 1;

pub const INTERNAL_MODULE_REQUIRED: u64 = 1 << 0;
pub const INTERNAL_MODULE_COMPRESSED: u64 = 1 << 1;

pub const TPM_EVENT_LOG_FORMAT_TCG_2: u64 = 2;

/// Default stack size when the kernel does not ask for one (spec minimum).
pub const DEFAULT_STACK_SIZE: u64 = 64 * 1024;
/// Largest stack we agree to allocate, so a corrupt request can't eat RAM.
pub const MAX_STACK_SIZE: u64 = 64 * 1024 * 1024;
/// Most entropy values handed out in one response.
pub const MAX_ENTROPY_VALUES: u64 = 4096;

/// Where the executable must live: the top 2 GiB of the address space.
pub const KERNEL_BASE_MIN: u64 = 0xffff_ffff_8000_0000;

// ---- Response layouts (all `repr(C)`, exactly as in the spec) ----

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Uuid {
    pub a: u32,
    pub b: u16,
    pub c: u16,
    pub d: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct File {
    pub revision: u64,
    pub address: u64,
    pub size: u64,
    pub path: u64,
    pub string: u64,
    pub media_type: u32,
    pub unused: u32,
    pub tftp_ipv4: [u8; 4],
    pub tftp_port: u32,
    pub partition_index: u32,
    pub mbr_disk_id: u32,
    pub gpt_disk_uuid: Uuid,
    pub gpt_part_uuid: Uuid,
    pub part_uuid: Uuid,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    pub typ: u64,
}

impl MemmapEntry {
    pub const fn end(&self) -> u64 {
        self.base + self.length
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Framebuffer {
    pub address: u64,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
    pub unused: [u8; 7],
    pub edid_size: u64,
    pub edid: u64,
    // Response revision 1
    pub mode_count: u64,
    pub modes: u64,
    // Response revision 2
    pub upright_rotation: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VideoMode {
    pub pitch: u64,
    pub width: u64,
    pub height: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

/// The common head of every request.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RequestHeader {
    pub id: [u64; 4],
    pub revision: u64,
    pub response: u64,
}

pub const REQUEST_HEADER_SIZE: usize = core::mem::size_of::<RequestHeader>();

/// How many bytes of each request we read, for the fields we understand
/// (header plus optional members, for the highest request revision we know).
pub fn request_size(feature: Feature, revision: u64) -> usize {
    REQUEST_HEADER_SIZE
        + match feature {
            Feature::StackSize => 8,
            Feature::PagingMode => {
                if revision >= 1 {
                    24
                } else {
                    8
                }
            }
            Feature::Mp => 8,
            Feature::EntryPoint => 8,
            Feature::Module => {
                if revision >= 1 {
                    16
                } else {
                    0
                }
            }
            Feature::Entropy => 8,
            _ => 0,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn layouts_match_spec() {
        assert_eq!(size_of::<Uuid>(), 16);
        assert_eq!(offset_of!(File, media_type), 40);
        assert_eq!(offset_of!(File, tftp_ipv4), 48);
        assert_eq!(offset_of!(File, tftp_port), 52);
        assert_eq!(offset_of!(File, partition_index), 56);
        assert_eq!(offset_of!(File, mbr_disk_id), 60);
        assert_eq!(offset_of!(File, gpt_disk_uuid), 64);
        assert_eq!(size_of::<File>(), 112);
        assert_eq!(size_of::<MemmapEntry>(), 24);
        assert_eq!(offset_of!(Framebuffer, bpp), 32);
        assert_eq!(offset_of!(Framebuffer, unused), 41);
        assert_eq!(offset_of!(Framebuffer, edid_size), 48);
        assert_eq!(offset_of!(Framebuffer, mode_count), 64);
        assert_eq!(size_of::<Framebuffer>(), 88);
        assert_eq!(offset_of!(VideoMode, bpp), 24);
        assert_eq!(size_of::<VideoMode>(), 40);
        assert_eq!(size_of::<RequestHeader>(), 48);
    }

    #[test]
    fn ids_are_unique() {
        for (i, (_, a)) in FEATURES.iter().enumerate() {
            for (_, b) in &FEATURES[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
