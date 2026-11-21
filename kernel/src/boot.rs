//! Limine boot protocol requests. Limine fills in the responses before it
//! jumps to `kmain`.

use limine::BaseRevision;
use limine::request::{
    BootloaderInfoRequest, ExecutableCmdlineRequest, FramebufferRequest, MemoryMapRequest,
    RequestsEndMarker, RequestsStartMarker,
};

#[used]
#[unsafe(link_section = ".requests")]
pub static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static BOOTLOADER_INFO: BootloaderInfoRequest = BootloaderInfoRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static MEMORY_MAP: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static CMDLINE: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

/// The kernel command line from limine.conf, or "" if none.
pub fn cmdline() -> &'static str {
    CMDLINE
        .get_response()
        .and_then(|r| r.cmdline().to_str().ok())
        .unwrap_or("")
}

/// Value of `key=value` on the command line, if present.
pub fn arg<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split_ascii_whitespace()
        .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
}
