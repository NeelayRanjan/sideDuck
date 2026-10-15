use std::{fmt, io};

/// A sparse tile address. Layer identifiers are stable; coordinates are unsigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileKey {
    pub layer: u32,
    pub x: u32,
    pub y: u32,
}

/// Premultiplied linear RGBA bytes. Float components use little-endian IEEE binary16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PixelFormat {
    Rgba8 = 1,
    Rgba16Float = 2,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 => 4,
            Self::Rgba16Float => 8,
        }
    }

    pub(crate) fn is_transparent(self, bytes: &[u8]) -> bool {
        match self {
            Self::Rgba8 => bytes.chunks_exact(4).all(|pixel| pixel[3] == 0),
            Self::Rgba16Float => bytes
                .chunks_exact(8)
                .all(|pixel| u16::from_le_bytes([pixel[6], pixel[7]]) & 0x7fff == 0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreOptions {
    pub tile_size: u32,
    pub format: PixelFormat,
    /// Ceiling for decoded tile payloads, not process RSS or index metadata.
    pub cache_budget_bytes: usize,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            tile_size: 256,
            format: PixelFormat::Rgba8,
            cache_budget_bytes: 64 * 1024 * 1024,
        }
    }
}

impl StoreOptions {
    pub fn tile_bytes(self) -> Result<usize> {
        // This bounds compression scratch and mapped windows as well as arithmetic.
        if self.tile_size == 0 || self.tile_size > 4096 {
            return Err(Error::InvalidOptions("tile size must be in 1..=4096"));
        }
        let size = usize::try_from(self.tile_size)
            .map_err(|_| Error::InvalidOptions("tile size does not fit this host"))?;
        size.checked_mul(size)
            .and_then(|pixels| pixels.checked_mul(self.format.bytes_per_pixel()))
            .ok_or(Error::InvalidOptions("tile byte size overflow"))
    }

    pub(crate) fn validate(self) -> Result<usize> {
        let bytes = self.tile_bytes()?;
        if self.cache_budget_bytes < bytes {
            return Err(Error::BudgetTooSmall {
                required: bytes,
                budget: self.cache_budget_bytes,
            });
        }
        Ok(bytes)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StoreStats {
    pub resident_bytes: usize,
    pub peak_resident_bytes: usize,
    /// Reserved asynchronous decoded payload; cache plus reservations never exceeds its ceiling.
    pub prefetch_reserved_bytes: usize,
    pub peak_total_resident_bytes: usize,
    pub cache_entries: usize,
    pub persisted_tiles: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub evictions: u64,
    pub compressed_bytes_read: u64,
    pub compressed_bytes_written: u64,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidOptions(&'static str),
    Corrupt(&'static str),
    FormatMismatch,
    TileLength { expected: usize, actual: usize },
    BudgetTooSmall { required: usize, budget: usize },
    AllTilesPinned,
    AlreadyLocked,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::InvalidOptions(reason) => write!(formatter, "invalid store options: {reason}"),
            Self::Corrupt(reason) => write!(formatter, "invalid tile store: {reason}"),
            Self::FormatMismatch => formatter.write_str("tile format or size differs from store"),
            Self::TileLength { expected, actual } => {
                write!(formatter, "tile has {actual} bytes; expected {expected}")
            }
            Self::BudgetTooSmall { required, budget } => {
                write!(
                    formatter,
                    "tile requires {required} bytes; cache budget is {budget}"
                )
            }
            Self::AllTilesPinned => formatter.write_str("all eviction candidates are pinned"),
            Self::AlreadyLocked => formatter.write_str("tile store is already open for writing"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
