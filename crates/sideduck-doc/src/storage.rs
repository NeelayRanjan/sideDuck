//! Bounded, checksummed publication primitives. Only complete log frames commit.

use crate::model::{ContentId, DocumentState, History, Operation, TileChange};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(crate) const LOG_MAGIC: &[u8; 8] = b"OPSLOG01";
const COMMIT_MAGIC: &[u8; 8] = b"COMMIT01";
const OBJECT_MAGIC: &[u8; 4] = b"OBJ1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub version: u32,
    pub sequence: u64,
    pub previous: ContentId,
    pub content_id: ContentId,
    pub operation: Operation,
    pub state: DocumentState,
    pub changes: Vec<TileChange>,
    pub index_hash: ContentId,
    pub checkpoint: bool,
    pub history: History,
}

#[derive(Clone, Debug)]
pub(crate) struct LocatedRecord {
    pub record: Record,
    pub hash: ContentId,
    pub end: u64,
}

pub(crate) fn hash(bytes: &[u8]) -> ContentId {
    Sha256::digest(bytes).into()
}

pub(crate) fn hex(id: ContentId) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in id {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 15) as usize] as char);
    }
    output
}

pub(crate) fn index_path(directory: &Path, sequence: u64) -> PathBuf {
    directory
        .join("checkpoints")
        .join(format!("{sequence:020}.index"))
}

pub(crate) fn sync_directory(directory: &Path) -> Result<()> {
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("next");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    sync_directory(
        path.parent()
            .ok_or(Error::Invalid("file has no parent directory"))?,
    )
}

pub(crate) fn copy_durable(source: &Path, destination: &Path) -> Result<ContentId> {
    let temporary = destination.with_extension("next");
    fs::copy(source, &temporary)?;
    let mut file = OpenOptions::new().read(true).write(true).open(&temporary)?;
    file.sync_all()?;
    let digest = hash_reader(&mut file)?;
    fs::rename(&temporary, destination)?;
    sync_directory(
        destination
            .parent()
            .ok_or(Error::Invalid("file has no parent directory"))?,
    )?;
    Ok(digest)
}

pub(crate) fn hash_reader(reader: &mut impl Read) -> Result<ContentId> {
    let mut state = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        state.update(&buffer[..count]);
    }
    Ok(state.finalize().into())
}

pub(crate) fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > maximum as u64 {
        return Err(Error::Limit("file exceeds its configured byte bound"));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(Error::Limit("file grew beyond its byte bound"));
    }
    Ok(bytes)
}

pub(crate) fn content_id(operation: &Operation, changes: &[TileChange]) -> Result<ContentId> {
    Ok(hash(&json_bytes(&(operation, changes), 64 * 1024 * 1024)?))
}

pub(crate) fn json_bytes(value: &impl Serialize, maximum: usize) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        maximum: usize,
        exceeded: bool,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
                self.exceeded = true;
                return Err(std::io::Error::other("serialized data exceeds byte bound"));
            }
            if self.bytes.capacity() - self.bytes.len() < bytes.len() {
                self.bytes.reserve_exact(bytes.len());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        maximum,
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut writer, value);
    if writer.exceeded {
        return Err(Error::Limit(
            "serialized data exceeds its configured byte bound",
        ));
    }
    result?;
    Ok(writer.bytes)
}

pub(crate) fn append(file: &mut File, record: &Record, maximum: usize) -> Result<LocatedRecord> {
    let bytes = json_bytes(record, maximum)?;
    let digest = hash(&bytes);
    file.seek(SeekFrom::End(0))?;
    file.write_all(&(bytes.len() as u32).to_le_bytes())?;
    file.write_all(&bytes)?;
    file.write_all(&digest)?;
    file.write_all(COMMIT_MAGIC)?;
    file.sync_all()?;
    Ok(LocatedRecord {
        record: record.clone(),
        hash: digest,
        end: file.stream_position()?,
    })
}

/// Visit records one at a time. A torn final frame is ignored; corruption with
/// later bytes is rejected rather than discarding subsequent committed history.
pub(crate) fn scan(
    path: &Path,
    maximum: usize,
    mut visit: impl FnMut(&LocatedRecord) -> Result<()>,
) -> Result<LocatedRecord> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)
        .map_err(|_| Error::Corrupt("truncated operation log header"))?;
    if &magic != LOG_MAGIC {
        return Err(Error::Corrupt("operation log magic"));
    }
    let mut last: Option<LocatedRecord> = None;
    while file.stream_position()? < length {
        let start = file.stream_position()?;
        let mut size = [0_u8; 4];
        if file.read_exact(&mut size).is_err() {
            break;
        }
        let size = u32::from_le_bytes(size) as usize;
        if size > maximum {
            return Err(Error::Limit(
                "operation log frame exceeds its configured byte bound",
            ));
        }
        let end = start
            .checked_add(size as u64 + 44)
            .ok_or(Error::Corrupt("operation log offset overflow"))?;
        if end > length {
            break;
        }
        let mut bytes = vec![0; size];
        file.read_exact(&mut bytes)?;
        let mut digest = [0_u8; 32];
        let mut commit = [0_u8; 8];
        file.read_exact(&mut digest)?;
        file.read_exact(&mut commit)?;
        if &commit != COMMIT_MAGIC || hash(&bytes) != digest {
            if end == length {
                break;
            }
            return Err(Error::Corrupt("operation log checksum or commit marker"));
        }
        let record: Record = serde_json::from_slice(&bytes)?;
        let expected_sequence = last.as_ref().map_or(0, |last| last.record.sequence + 1);
        let previous = last.as_ref().map_or([0; 32], |last| last.hash);
        if record.version != 1
            || record.sequence != expected_sequence
            || record.previous != previous
            || content_id(&record.operation, &record.changes)? != record.content_id
        {
            return Err(Error::Corrupt("operation log ordering or content address"));
        }
        record.state.validate()?;
        let mut history_sequences = std::collections::BTreeSet::new();
        for entry in record.history.undo.iter().chain(&record.history.redo) {
            if entry.sequence == 0
                || entry.sequence > record.sequence
                || !history_sequences.insert(entry.sequence)
            {
                return Err(Error::Corrupt("invalid undo journal sequence"));
            }
        }
        let located = LocatedRecord {
            record,
            hash: digest,
            end,
        };
        visit(&located)?;
        last = Some(located);
    }
    last.ok_or(Error::Corrupt(
        "operation log has no committed initial state",
    ))
}

pub(crate) fn find(path: &Path, maximum: usize, sequence: u64) -> Result<LocatedRecord> {
    let mut found = None;
    scan(path, maximum, |record| {
        if record.record.sequence == sequence {
            found = Some(record.clone());
        }
        Ok(())
    })?;
    found.ok_or(Error::Invalid("operation sequence does not exist"))
}

pub(crate) fn put_object(directory: &Path, pixels: &[u8]) -> Result<ContentId> {
    let id = hash(pixels);
    let path = directory.join("objects").join(format!("{}.tile", hex(id)));
    if path.exists() {
        // Validate an existing object before trusting content-addressed deduplication.
        let existing = read_object(directory, id, pixels.len())?;
        if existing != pixels {
            return Err(Error::Corrupt("tile object content address collision"));
        }
        return Ok(id);
    }
    let compressed = lz4_flex::block::compress(pixels);
    let use_compressed = compressed.len() < pixels.len();
    let payload = if use_compressed {
        compressed.as_slice()
    } else {
        pixels
    };
    let mut bytes = Vec::with_capacity(payload.len() + 9);
    bytes.extend_from_slice(OBJECT_MAGIC);
    bytes.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
    bytes.push(u8::from(use_compressed));
    bytes.extend_from_slice(payload);
    atomic_write(&path, &bytes)?;
    Ok(id)
}

pub(crate) fn read_object(directory: &Path, id: ContentId, expected: usize) -> Result<Vec<u8>> {
    let bytes = read_bounded(
        &directory.join("objects").join(format!("{}.tile", hex(id))),
        expected + 9,
    )?;
    if bytes.len() < 9
        || &bytes[..4] != OBJECT_MAGIC
        || u32::from_le_bytes(bytes[4..8].try_into().expect("four bytes")) as usize != expected
    {
        return Err(Error::Corrupt("tile object header or length"));
    }
    let pixels = match bytes[8] {
        0 if bytes.len() == expected + 9 => bytes[9..].to_vec(),
        1 => {
            let mut pixels = vec![0; expected];
            let count = lz4_flex::block::decompress_into(&bytes[9..], &mut pixels)
                .map_err(|_| Error::Corrupt("tile object compression"))?;
            if count != expected {
                return Err(Error::Corrupt("tile object decoded length"));
            }
            pixels
        }
        _ => return Err(Error::Corrupt("tile object codec")),
    };
    if hash(&pixels) != id {
        return Err(Error::Corrupt("tile object content checksum"));
    }
    Ok(pixels)
}
