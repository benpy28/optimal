//! UEFI definitions, written from the UEFI 2.10 specification. Only what the
//! loader uses is typed; unused table slots are kept as plain pointers so the
//! layouts stay exact.

#![allow(dead_code)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Event = *mut c_void;
pub type Status = usize;

pub const SUCCESS: Status = 0;
const ERR: usize = 1 << 63;
pub const LOAD_ERROR: Status = ERR | 1;
pub const INVALID_PARAMETER: Status = ERR | 2;
pub const UNSUPPORTED: Status = ERR | 3;
pub const BUFFER_TOO_SMALL: Status = ERR | 5;
pub const NOT_READY: Status = ERR | 6;
pub const DEVICE_ERROR: Status = ERR | 7;
pub const OUT_OF_RESOURCES: Status = ERR | 9;
pub const NOT_FOUND: Status = ERR | 14;
pub const ACCESS_DENIED: Status = ERR | 15;
pub const SECURITY_VIOLATION: Status = ERR | 26;

pub fn is_error(s: Status) -> bool {
    s & ERR != 0
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

pub const LOADED_IMAGE_GUID: Guid = Guid(
    0x5b1b31a1,
    0x9562,
    0x11d2,
    [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const SIMPLE_FILE_SYSTEM_GUID: Guid = Guid(
    0x964e5b22,
    0x6459,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const FILE_INFO_GUID: Guid = Guid(
    0x09576e92,
    0x6d3f,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const DEVICE_PATH_GUID: Guid = Guid(
    0x09576e91,
    0x6d3f,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const BLOCK_IO_GUID: Guid = Guid(
    0x964e5b21,
    0x6459,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const GOP_GUID: Guid = Guid(
    0x9042a9de,
    0x23dc,
    0x4a38,
    [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
);
pub const EDID_ACTIVE_GUID: Guid = Guid(
    0xbd8c1056,
    0x9f36,
    0x44ec,
    [0x92, 0xa8, 0xa6, 0x33, 0x7f, 0x81, 0x79, 0x86],
);
pub const EDID_DISCOVERED_GUID: Guid = Guid(
    0x1c0c34f6,
    0xd380,
    0x41fa,
    [0xa0, 0x49, 0x8a, 0xd0, 0x6c, 0x1a, 0x66, 0xaa],
);
pub const RNG_GUID: Guid = Guid(
    0x3152bca5,
    0xeade,
    0x433d,
    [0x86, 0x2e, 0xc0, 0x1c, 0xdc, 0x29, 0x1f, 0x44],
);
pub const TCG2_GUID: Guid = Guid(
    0x607f766c,
    0x7455,
    0x42be,
    [0x93, 0x0b, 0xe4, 0xd7, 0x6d, 0xb2, 0x72, 0x0f],
);
pub const ACPI_20_TABLE_GUID: Guid = Guid(
    0x8868e871,
    0xe4f1,
    0x11d3,
    [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81],
);
pub const ACPI_10_TABLE_GUID: Guid = Guid(
    0xeb9d2d30,
    0x2d88,
    0x11d3,
    [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d],
);
pub const SMBIOS_TABLE_GUID: Guid = Guid(
    0xeb9d2d31,
    0x2d88,
    0x11d3,
    [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d],
);
pub const SMBIOS3_TABLE_GUID: Guid = Guid(
    0xf2fd1544,
    0x9794,
    0x4a2c,
    [0x99, 0x2e, 0xe5, 0xbb, 0xcf, 0x20, 0xe3, 0x94],
);
pub const GLOBAL_VARIABLE_GUID: Guid = Guid(
    0x8be4df61,
    0x93ca,
    0x11d2,
    [0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c],
);

#[repr(C)]
pub struct TableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct ConfigurationTable {
    pub guid: Guid,
    pub table: *mut c_void,
}

#[repr(C)]
pub struct SystemTable {
    pub hdr: TableHeader,
    pub firmware_vendor: *const u16,
    pub firmware_revision: u32,
    pub console_in_handle: Handle,
    pub con_in: *mut SimpleTextInput,
    pub console_out_handle: Handle,
    pub con_out: *mut SimpleTextOutput,
    pub standard_error_handle: Handle,
    pub std_err: *mut SimpleTextOutput,
    pub runtime_services: *mut RuntimeServices,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *mut ConfigurationTable,
}

pub const SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;

#[repr(C)]
pub struct InputKey {
    pub scan_code: u16,
    pub unicode_char: u16,
}

#[repr(C)]
pub struct SimpleTextInput {
    pub reset: extern "efiapi" fn(*mut SimpleTextInput, bool) -> Status,
    pub read_key_stroke: extern "efiapi" fn(*mut SimpleTextInput, *mut InputKey) -> Status,
    pub wait_for_key: Event,
}

#[repr(C)]
pub struct SimpleTextOutput {
    pub reset: extern "efiapi" fn(*mut SimpleTextOutput, bool) -> Status,
    pub output_string: extern "efiapi" fn(*mut SimpleTextOutput, *const u16) -> Status,
    pub test_string: *const c_void,
    pub query_mode: *const c_void,
    pub set_mode: *const c_void,
    pub set_attribute: extern "efiapi" fn(*mut SimpleTextOutput, usize) -> Status,
    pub clear_screen: extern "efiapi" fn(*mut SimpleTextOutput) -> Status,
    pub set_cursor_position: *const c_void,
    pub enable_cursor: extern "efiapi" fn(*mut SimpleTextOutput, bool) -> Status,
    pub mode: *const c_void,
}

#[repr(C)]
pub struct Time {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub pad1: u8,
    pub nanosecond: u32,
    pub time_zone: i16,
    pub daylight: u8,
    pub pad2: u8,
}

#[repr(C)]
pub struct RuntimeServices {
    pub hdr: TableHeader,
    pub get_time: extern "efiapi" fn(*mut Time, *mut c_void) -> Status,
    pub set_time: *const c_void,
    pub get_wakeup_time: *const c_void,
    pub set_wakeup_time: *const c_void,
    pub set_virtual_address_map: *const c_void,
    pub convert_pointer: *const c_void,
    pub get_variable:
        extern "efiapi" fn(*const u16, *const Guid, *mut u32, *mut usize, *mut c_void) -> Status,
    pub get_next_variable_name: *const c_void,
    pub set_variable: *const c_void,
    pub get_next_high_monotonic_count: *const c_void,
    pub reset_system: extern "efiapi" fn(u32, Status, usize, *const c_void) -> !,
    pub update_capsule: *const c_void,
    pub query_capsule_capabilities: *const c_void,
    pub query_variable_info: *const c_void,
}

pub const RESET_COLD: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryDescriptor {
    pub typ: u32,
    pub pad: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

pub const ALLOCATE_ANY_PAGES: u32 = 0;
pub const ALLOCATE_MAX_ADDRESS: u32 = 1;
pub const ALLOCATE_ADDRESS: u32 = 2;

pub const LOADER_CODE: u32 = 1;
pub const LOADER_DATA: u32 = 2;

pub const OPEN_PROTOCOL_GET_PROTOCOL: u32 = 2;
pub const LOCATE_BY_PROTOCOL: u32 = 2;

#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,
    pub raise_tpl: *const c_void,
    pub restore_tpl: *const c_void,
    pub allocate_pages: extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: extern "efiapi" fn(u64, usize) -> Status,
    pub get_memory_map: extern "efiapi" fn(
        *mut usize,
        *mut MemoryDescriptor,
        *mut usize,
        *mut usize,
        *mut u32,
    ) -> Status,
    pub allocate_pool: extern "efiapi" fn(u32, usize, *mut *mut u8) -> Status,
    pub free_pool: extern "efiapi" fn(*mut u8) -> Status,
    pub create_event: *const c_void,
    pub set_timer: *const c_void,
    pub wait_for_event: extern "efiapi" fn(usize, *const Event, *mut usize) -> Status,
    pub signal_event: *const c_void,
    pub close_event: *const c_void,
    pub check_event: extern "efiapi" fn(Event) -> Status,
    pub install_protocol_interface: *const c_void,
    pub reinstall_protocol_interface: *const c_void,
    pub uninstall_protocol_interface: *const c_void,
    pub handle_protocol: extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: *const c_void,
    pub register_protocol_notify: *const c_void,
    pub locate_handle: *const c_void,
    pub locate_device_path:
        extern "efiapi" fn(*const Guid, *mut *const DevicePath, *mut Handle) -> Status,
    pub install_configuration_table: *const c_void,
    pub load_image: extern "efiapi" fn(
        bool,
        Handle,
        *const DevicePath,
        *const c_void,
        usize,
        *mut Handle,
    ) -> Status,
    pub start_image: extern "efiapi" fn(Handle, *mut usize, *mut *mut u16) -> Status,
    pub exit: *const c_void,
    pub unload_image: *const c_void,
    pub exit_boot_services: extern "efiapi" fn(Handle, usize) -> Status,
    pub get_next_monotonic_count: *const c_void,
    pub stall: extern "efiapi" fn(usize) -> Status,
    pub set_watchdog_timer: extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    pub connect_controller: *const c_void,
    pub disconnect_controller: *const c_void,
    pub open_protocol:
        extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void, Handle, Handle, u32) -> Status,
    pub close_protocol: *const c_void,
    pub open_protocol_information: *const c_void,
    pub protocols_per_handle: *const c_void,
    pub locate_handle_buffer:
        extern "efiapi" fn(u32, *const Guid, *const c_void, *mut usize, *mut *mut Handle) -> Status,
    pub locate_protocol: extern "efiapi" fn(*const Guid, *const c_void, *mut *mut c_void) -> Status,
    pub install_multiple_protocol_interfaces: *const c_void,
    pub uninstall_multiple_protocol_interfaces: *const c_void,
    pub calculate_crc32: extern "efiapi" fn(*const c_void, usize, *mut u32) -> Status,
    pub copy_mem: *const c_void,
    pub set_mem: *const c_void,
    pub create_event_ex: *const c_void,
}

#[repr(C)]
pub struct LoadedImage {
    pub revision: u32,
    pub parent_handle: Handle,
    pub system_table: *mut SystemTable,
    pub device_handle: Handle,
    pub file_path: *const DevicePath,
    pub reserved: *const c_void,
    pub load_options_size: u32,
    pub load_options: *const c_void,
    pub image_base: *mut c_void,
    pub image_size: u64,
    pub image_code_type: u32,
    pub image_data_type: u32,
    pub unload: *const c_void,
}

#[repr(C)]
pub struct DevicePath {
    pub typ: u8,
    pub sub_type: u8,
    pub length: [u8; 2],
}

#[repr(C)]
pub struct SimpleFileSystem {
    pub revision: u64,
    pub open_volume: extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut FileProtocol) -> Status,
}

#[repr(C)]
pub struct FileProtocol {
    pub revision: u64,
    pub open: extern "efiapi" fn(
        *mut FileProtocol,
        *mut *mut FileProtocol,
        *const u16,
        u64,
        u64,
    ) -> Status,
    pub close: extern "efiapi" fn(*mut FileProtocol) -> Status,
    pub delete: *const c_void,
    pub read: extern "efiapi" fn(*mut FileProtocol, *mut usize, *mut u8) -> Status,
    pub write: *const c_void,
    pub get_position: *const c_void,
    pub set_position: *const c_void,
    pub get_info: extern "efiapi" fn(*mut FileProtocol, *const Guid, *mut usize, *mut u8) -> Status,
    pub set_info: *const c_void,
    pub flush: *const c_void,
}

pub const FILE_MODE_READ: u64 = 1;
pub const FILE_DIRECTORY: u64 = 0x10;

#[repr(C)]
pub struct BlockIoMedia {
    pub media_id: u32,
    pub removable_media: bool,
    pub media_present: bool,
    pub logical_partition: bool,
    pub read_only: bool,
    pub write_caching: bool,
    pub block_size: u32,
    pub io_align: u32,
    pub last_block: u64,
}

#[repr(C)]
pub struct BlockIo {
    pub revision: u64,
    pub media: *const BlockIoMedia,
    pub reset: *const c_void,
    pub read_blocks: extern "efiapi" fn(*mut BlockIo, u32, u64, usize, *mut u8) -> Status,
    pub write_blocks: *const c_void,
    pub flush_blocks: *const c_void,
}

#[repr(C)]
pub struct GraphicsOutput {
    pub query_mode:
        extern "efiapi" fn(*mut GraphicsOutput, u32, *mut usize, *mut *const GopModeInfo) -> Status,
    pub set_mode: extern "efiapi" fn(*mut GraphicsOutput, u32) -> Status,
    pub blt: *const c_void,
    pub mode: *const GopMode,
}

#[repr(C)]
pub struct GopMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *const GopModeInfo,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GopModeInfo {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub reserved_mask: u32,
    pub pixels_per_scan_line: u32,
}

pub const PIXEL_RGBX: u32 = 0;
pub const PIXEL_BGRX: u32 = 1;
pub const PIXEL_BITMASK: u32 = 2;
pub const PIXEL_BLT_ONLY: u32 = 3;

#[repr(C)]
pub struct EdidProtocol {
    pub size_of_edid: u32,
    pub edid: *const u8,
}

#[repr(C)]
pub struct RngProtocol {
    pub get_info: *const c_void,
    pub get_rng: extern "efiapi" fn(*mut RngProtocol, *const Guid, usize, *mut u8) -> Status,
}

/// EFI_TCG2_BOOT_SERVICE_CAPABILITY. Natural alignment, NOT packed: firmware
/// (edk2) lays it out with padding, 36 bytes, and a caller passing a smaller
/// size gets the 28-byte TrEE 1.0 layout back instead.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Tcg2Capability {
    pub size: u8,
    pub structure_version: [u8; 2],
    pub protocol_version: [u8; 2],
    pub hash_algorithm_bitmap: u32,
    pub supported_event_logs: u32,
    pub tpm_present_flag: u8,
    pub max_command_size: u16,
    pub max_response_size: u16,
    pub manufacturer_id: u32,
    pub number_of_pcr_banks: u32,
    pub active_pcr_banks: u32,
}

// The layout firmware uses (edk2 MdePkg/Include/Protocol/Tcg2Protocol.h).
const _: () = {
    assert!(core::mem::size_of::<Tcg2Capability>() == 36);
    assert!(core::mem::offset_of!(Tcg2Capability, hash_algorithm_bitmap) == 8);
    assert!(core::mem::offset_of!(Tcg2Capability, tpm_present_flag) == 16);
    assert!(core::mem::offset_of!(Tcg2Capability, active_pcr_banks) == 32);
};

#[repr(C)]
pub struct Tcg2Protocol {
    pub get_capability: extern "efiapi" fn(*mut Tcg2Protocol, *mut Tcg2Capability) -> Status,
    pub get_event_log:
        extern "efiapi" fn(*mut Tcg2Protocol, u32, *mut u64, *mut u64, *mut bool) -> Status,
    pub hash_log_extend_event:
        extern "efiapi" fn(*mut Tcg2Protocol, u64, u64, u64, *const u8) -> Status,
    pub submit_command:
        extern "efiapi" fn(*mut Tcg2Protocol, u32, *const u8, u32, *mut u8) -> Status,
    pub get_active_pcr_banks: *const c_void,
    pub set_active_pcr_banks: *const c_void,
    pub get_result_of_set_active_pcr_banks: *const c_void,
}

pub const TCG2_EVENT_LOG_FORMAT_TCG_2: u32 = 0x2;
pub const TCG2_HASH_ALG_SHA256: u32 = 0x2;

/// Builds a NUL-terminated UCS-2 string from ASCII at compile time.
pub const fn ucs2<const N: usize>(s: &str) -> [u16; N] {
    let b = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i] as u16;
        i += 1;
    }
    out
}
