use crate::{Error, PixelFormat, Result, StoreOptions, TileKey};
use crc32fast::Hasher;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
};

pub(crate) const STORE_MAGIC: &[u8; 8] = b"TILEBLB1";
const INDEX_MAGIC: &[u8; 8] = b"TILEIDX1";
const HEADER_BYTES: u64 = 32;
const RECORD_BYTES: u64 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Record {
    pub generation: u64,
    pub offset: u64,
    pub length: u32,
    pub checksum: u32,
    pub codec: u8,
}

pub(crate) fn lock(file: &File) -> Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::AlreadyLocked),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(crate) fn read_index(
    directory: &Path,
    options: StoreOptions,
    store_length: u64,
) -> Result<(BTreeMap<TileKey, Record>, u64)> {
    let file = File::open(directory.join("tiles.index"))?;
    let length = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut header = [0_u8; HEADER_BYTES as usize];
    read_exact(&mut reader, &mut header)?;
    if &header[..8] != INDEX_MAGIC {
        return Err(Error::Corrupt("index magic or version"));
    }
    let tile_size = u32_at(&header, 8);
    let format = match u32_at(&header, 12) {
        1 => PixelFormat::Rgba8,
        2 => PixelFormat::Rgba16Float,
        _ => return Err(Error::Corrupt("pixel format")),
    };
    if tile_size != options.tile_size || format != options.format {
        return Err(Error::FormatMismatch);
    }
    let count = u64_at(&header, 16);
    let generation = u64_at(&header, 24);
    let expected_length = count
        .checked_mul(RECORD_BYTES)
        .and_then(|records| records.checked_add(HEADER_BYTES + 4))
        .ok_or(Error::Corrupt("index size overflow"))?;
    if expected_length != length {
        return Err(Error::Corrupt("index length"));
    }
    let raw_length = options.tile_bytes()? as u64;
    let mut index = BTreeMap::new();
    let mut checksum = Hasher::new();
    checksum.update(&header);
    for _ in 0..count {
        let mut bytes = [0_u8; RECORD_BYTES as usize];
        read_exact(&mut reader, &mut bytes)?;
        checksum.update(&bytes);
        let key = TileKey {
            layer: u32_at(&bytes, 0),
            x: u32_at(&bytes, 4),
            y: u32_at(&bytes, 8),
        };
        let record = Record {
            generation: u64_at(&bytes, 12),
            offset: u64_at(&bytes, 20),
            length: u32_at(&bytes, 28),
            checksum: u32_at(&bytes, 32),
            codec: bytes[36],
        };
        if record.offset < STORE_MAGIC.len() as u64
            || record.length == 0
            || u64::from(record.length) > raw_length
            || record.generation > generation
            || record.codec > 1
            || (record.codec == 0 && u64::from(record.length) != raw_length)
            || bytes[37..] != [0, 0, 0]
            || record
                .offset
                .checked_add(u64::from(record.length))
                .is_none_or(|end| end > store_length)
        {
            return Err(Error::Corrupt("tile record bounds or encoding"));
        }
        if index.insert(key, record).is_some() {
            return Err(Error::Corrupt("duplicate tile key"));
        }
    }
    let mut expected_checksum = [0; 4];
    read_exact(&mut reader, &mut expected_checksum)?;
    if checksum.finalize() != u32::from_le_bytes(expected_checksum) {
        return Err(Error::Corrupt("index checksum"));
    }
    Ok((index, generation))
}

pub(crate) fn write_index(
    directory: &Path,
    options: StoreOptions,
    index: &BTreeMap<TileKey, Record>,
    generation: u64,
) -> Result<()> {
    write_index_records(
        directory,
        options,
        index.len() as u64,
        index.iter().map(|(key, record)| (*key, *record)),
        generation,
    )
}

pub(crate) fn write_index_records(
    directory: &Path,
    options: StoreOptions,
    record_count: u64,
    records: impl IntoIterator<Item = (TileKey, Record)>,
    generation: u64,
) -> Result<()> {
    let temporary = directory.join("tiles.index.next");
    // The store lock serializes writers, including recovery from a prior .next file.
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    let mut writer = BufWriter::new(file);
    let mut checksum = Hasher::new();
    let mut header = [0; HEADER_BYTES as usize];
    header[..8].copy_from_slice(INDEX_MAGIC);
    header[8..12].copy_from_slice(&options.tile_size.to_le_bytes());
    header[12..16].copy_from_slice(&(options.format as u32).to_le_bytes());
    header[16..24].copy_from_slice(&record_count.to_le_bytes());
    header[24..32].copy_from_slice(&generation.to_le_bytes());
    writer.write_all(&header)?;
    checksum.update(&header);
    let mut written = 0_u64;
    for (key, record) in records {
        written += 1;
        let mut bytes = [0; RECORD_BYTES as usize];
        bytes[0..4].copy_from_slice(&key.layer.to_le_bytes());
        bytes[4..8].copy_from_slice(&key.x.to_le_bytes());
        bytes[8..12].copy_from_slice(&key.y.to_le_bytes());
        bytes[12..20].copy_from_slice(&record.generation.to_le_bytes());
        bytes[20..28].copy_from_slice(&record.offset.to_le_bytes());
        bytes[28..32].copy_from_slice(&record.length.to_le_bytes());
        bytes[32..36].copy_from_slice(&record.checksum.to_le_bytes());
        bytes[36] = record.codec;
        writer.write_all(&bytes)?;
        checksum.update(&bytes);
    }
    if written != record_count {
        return Err(Error::InvalidOptions("index iterator count mismatch"));
    }
    writer.write_all(&checksum.finalize().to_le_bytes())?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    fs::rename(&temporary, directory.join("tiles.index"))?;
    // Persist directory metadata after the atomic replacement on Linux and macOS.
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn read_exact(reader: &mut impl Read, buffer: &mut [u8]) -> Result<()> {
    reader.read_exact(buffer).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Corrupt("truncated index")
        } else {
            error.into()
        }
    })
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed field"))
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed field"))
}
