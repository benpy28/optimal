use crate::device::IoError;
use core::fmt;

/// Every failure the filesystem can report. Nothing in this crate panics on
/// bad on-disk data: corruption always surfaces as [`Error::Corrupt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The block device reported an error.
    Io(IoError),
    /// On-disk data failed a checksum, authentication or structural check.
    /// The string names the check, the number is the block address (or 0).
    Corrupt(&'static str, u64),
    /// No usable superblock was found (not an OptimalFS volume, or all copies damaged).
    NoFilesystem,
    /// The passphrase or key did not unlock any key slot.
    BadKey,
    /// The volume is encrypted but no key was supplied.
    KeyRequired,
    /// The on-disk format uses a feature or version this code does not know.
    Unsupported(&'static str),
    NoSpace,
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NotEmpty,
    NameTooLong,
    InvalidName,
    /// A caller-supplied argument was invalid (bad inode, offset overflow, ...).
    Invalid(&'static str),
    FileTooBig,
    TooManyLinks,
    /// The filesystem hit an error during commit and refuses further writes
    /// until it is remounted; the on-disk state is still the last good commit.
    Poisoned,
    ReadOnly,
    /// Commits kept failing to write (see [`Filesystem::commit`](crate::Filesystem::commit)).
    /// The open transaction is kept in memory and reads still work; new
    /// changes are refused until a commit succeeds. The on-disk state is
    /// the last good commit.
    DeviceFailing,
}

pub type Result<T> = core::result::Result<T, Error>;

impl From<IoError> for Error {
    fn from(e: IoError) -> Self {
        Error::Io(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e:?}"),
            Error::Corrupt(what, at) => write!(f, "corruption detected: {what} (block {at})"),
            Error::NoFilesystem => write!(f, "no valid OptimalFS superblock found"),
            Error::BadKey => write!(f, "wrong passphrase or key"),
            Error::KeyRequired => write!(f, "volume is encrypted, a key is required"),
            Error::Unsupported(w) => write!(f, "unsupported: {w}"),
            Error::NoSpace => write!(f, "no space left on device"),
            Error::NotFound => write!(f, "not found"),
            Error::Exists => write!(f, "already exists"),
            Error::NotDir => write!(f, "not a directory"),
            Error::IsDir => write!(f, "is a directory"),
            Error::NotEmpty => write!(f, "directory not empty"),
            Error::NameTooLong => write!(f, "name too long"),
            Error::InvalidName => write!(f, "invalid name"),
            Error::Invalid(w) => write!(f, "invalid argument: {w}"),
            Error::FileTooBig => write!(f, "file too big"),
            Error::TooManyLinks => write!(f, "too many links"),
            Error::Poisoned => write!(
                f,
                "filesystem stopped after an error it cannot undo; remount"
            ),
            Error::ReadOnly => write!(f, "read-only"),
            Error::DeviceFailing => write!(
                f,
                "the device keeps failing writes; changes are held in memory until a commit succeeds"
            ),
        }
    }
}
