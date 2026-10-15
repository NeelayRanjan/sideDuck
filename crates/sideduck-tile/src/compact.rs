//! Verified compact copies. Source files are never renamed, truncated or rewritten:
//! document checkpoints and undo logs may retain their physical blob offsets.
use crate::{persistence, read_record, Error, Result, TileStore};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const COPY_BUFFER_BYTES: usize = 64 * 1024;
const METADATA_BYTES: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionPhase {
    Started,
    BlobCopied,
    BlobsSynced,
    IndexSynced,
    Published,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactionReport {
    pub tiles: u64,
    pub source_bytes: u64,
    pub destination_bytes: u64,
    pub reclaimed_bytes: u64,
    pub index_bytes: u64,
    pub working_set_bound_bytes: usize,
}
struct TemporaryDirectory {
    path: PathBuf,
    published: bool,
}
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
impl TileStore {
    /// Copy the last committed live tiles into a NEW independent store, discarding
    /// obsolete/orphan blobs in that copy. Decoded CRCs are verified before each encoded
    /// blob is copied. Codec, pixel bytes and tile generations are preserved exactly.
    ///
    /// Destination publication is one atomic directory rename after both files sync.
    /// The source remains byte-for-byte unchanged at every boundary. This operation
    /// deliberately does NOT replace a document store: retained checkpoint indexes and
    /// log hashes still reference source offsets. A document must repackage all retained
    /// history or publish versioned stores before it can safely adopt this copy.
    ///
    /// The caller supplies a scratch ceiling for one mapped blob, one decoded tile,
    /// copy buffer and bounded metadata. The existing source index/cache are separate.
    /// No second full index or collection of decoded tiles is allocated.
    pub fn compact_copy(
        &mut self,
        destination: impl AsRef<Path>,
        max_working_bytes: usize,
    ) -> Result<CompactionReport> {
        self.compact_copy_with_observer(destination, max_working_bytes, |_| Ok(()))
    }
    /// Observer boundaries permit failure/forced-termination testing. An error at or
    /// after Published can leave a complete valid destination; the source is always intact.
    pub fn compact_copy_with_observer(
        &mut self,
        destination: impl AsRef<Path>,
        max_working_bytes: usize,
        mut observer: impl FnMut(CompactionPhase) -> io::Result<()>,
    ) -> Result<CompactionReport> {
        if self.has_uncommitted_changes() {
            return Err(Error::InvalidOptions(
                "compact-copy requires a committed source",
            ));
        }
        let bound = self
            .tile_bytes
            .checked_mul(2)
            .and_then(|value| value.checked_add(COPY_BUFFER_BYTES + METADATA_BYTES))
            .ok_or(Error::InvalidOptions("compaction working set overflow"))?;
        if bound > max_working_bytes {
            return Err(Error::BudgetTooSmall {
                required: bound,
                budget: max_working_bytes,
            });
        }
        let destination = destination.as_ref();
        if destination.exists() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
        }
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        self.cancel_prefetch()?;
        let mut temporary = temporary_directory(parent)?;
        observer(CompactionPhase::Started)?;
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(temporary.path.join("tiles.store"))?;
        persistence::lock(&output)?;
        output.write_all(persistence::STORE_MAGIC)?;
        let mut source = self.file.try_clone()?;
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
        let mut destination_bytes = persistence::STORE_MAGIC.len() as u64;
        for record in self.index.values().copied() {
            // The map and decoded allocation drop before the bounded copy buffer is used.
            // The exclusive store lock and immutable append-only source keep both reads identical.
            drop(read_record(&source, record, self.tile_bytes)?);
            source.seek(SeekFrom::Start(record.offset))?;
            let mut remaining = record.length as usize;
            while remaining > 0 {
                let count = remaining.min(buffer.len());
                source.read_exact(&mut buffer[..count])?;
                output.write_all(&buffer[..count])?;
                remaining -= count;
            }
            destination_bytes = destination_bytes
                .checked_add(u64::from(record.length))
                .ok_or(Error::InvalidOptions("compaction output size overflow"))?;
            observer(CompactionPhase::BlobCopied)?;
        }
        output.sync_all()?;
        observer(CompactionPhase::BlobsSynced)?;
        let records = self.index.iter().scan(
            persistence::STORE_MAGIC.len() as u64,
            |offset, (key, record)| {
                let mut relocated = *record;
                relocated.offset = *offset;
                *offset = offset
                    .checked_add(u64::from(record.length))
                    .expect("validated compact copy length");
                Some((*key, relocated))
            },
        );
        persistence::write_index_records(
            &temporary.path,
            self.options,
            self.index.len() as u64,
            records,
            self.generation,
        )?;
        observer(CompactionPhase::IndexSynced)?;
        let source_bytes = self.file.metadata()?.len();
        let index_bytes = fs::metadata(temporary.path.join("tiles.index"))?.len();
        drop(output);
        // A directory containing both durable files becomes visible in one rename.
        // Existing nonempty destinations cannot be overwritten; cooperating callers own
        // the destination path exclusively until this operation finishes.
        fs::rename(&temporary.path, destination)?;
        temporary.published = true;
        File::open(parent)?.sync_all()?;
        observer(CompactionPhase::Published)?;
        Ok(CompactionReport {
            tiles: self.index.len() as u64,
            source_bytes,
            destination_bytes,
            reclaimed_bytes: source_bytes.saturating_sub(destination_bytes),
            index_bytes,
            working_set_bound_bytes: bound,
        })
    }
}
fn temporary_directory(parent: &Path) -> Result<TemporaryDirectory> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    for _ in 0..1024 {
        let path = parent.join(format!(
            ".tile-compaction-{}-{}.tmp",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => {
                return Ok(TemporaryDirectory {
                    path,
                    published: false,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "temporary compaction directory collision",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PixelFormat, StoreOptions, TileKey};
    fn options() -> StoreOptions {
        StoreOptions {
            tile_size: 8,
            format: PixelFormat::Rgba8,
            cache_budget_bytes: 1024,
        }
    }
    fn key(layer: u32) -> TileKey {
        TileKey { layer, x: 0, y: 0 }
    }
    fn pixels(value: u8) -> Vec<u8> {
        [value, 2, 3, 255].repeat(64)
    }
    fn source(path: &Path) -> TileStore {
        let mut store = TileStore::create(path, options()).unwrap();
        for revision in 0..5 {
            for index in 0..4 {
                store
                    .put(key(index), &pixels(revision + index as u8))
                    .unwrap();
            }
            store.commit().unwrap();
        }
        store.remove(key(3));
        store.commit().unwrap();
        store
    }
    #[test]
    fn compaction_reclaims_obsolete_blobs_preserves_crc_generations_and_old_checkpoint_offsets() {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("original");
        let destination = root.path().join("compacted");
        let mut store = source(&origin);
        let original_store = fs::read(origin.join("tiles.store")).unwrap();
        let original_index = fs::read(origin.join("tiles.index")).unwrap();
        let generations: Vec<_> = store
            .index
            .values()
            .map(|record| record.generation)
            .collect();
        let report = store.compact_copy(&destination, 1 << 20).unwrap();
        assert_eq!(report.tiles, 3);
        assert!(report.reclaimed_bytes > 0);
        assert!(report.destination_bytes < report.source_bytes);
        assert!(report.working_set_bound_bytes <= 1 << 20);
        assert_eq!(
            fs::read(origin.join("tiles.store")).unwrap(),
            original_store
        );
        assert_eq!(
            fs::read(origin.join("tiles.index")).unwrap(),
            original_index
        );
        let mut compacted = TileStore::open(&destination, options()).unwrap();
        assert_eq!(
            compacted
                .index
                .values()
                .map(|record| record.generation)
                .collect::<Vec<_>>(),
            generations
        );
        for index in 0..3 {
            assert_eq!(
                compacted.get(key(index)).unwrap().unwrap(),
                pixels(4 + index as u8)
            );
        }
        assert!(compacted.get(key(3)).unwrap().is_none());
        assert!(store.compact_copy(&destination, 1 << 20).is_err());
        assert!(store.compact_copy(root.path().join("small"), 1).is_err());
        store.put(key(2), &pixels(99)).unwrap();
        assert!(store
            .compact_copy(root.path().join("dirty"), 1 << 20)
            .is_err());
        assert!(!root.path().join("dirty").exists());
    }
    #[test]
    fn compaction_empty_float_store_and_corrupt_blob_never_publish_invalid_destination() {
        let root = tempfile::tempdir().unwrap();
        let source_path = root.path().join("source");
        let mut float_options = options();
        float_options.format = PixelFormat::Rgba16Float;
        let mut store = TileStore::create(&source_path, float_options).unwrap();
        let empty = root.path().join("empty");
        assert_eq!(store.compact_copy(&empty, 1 << 20).unwrap().tiles, 0);
        assert_eq!(
            TileStore::open(&empty, float_options).unwrap().tile_count(),
            0
        );
        let half = [0, 0x30, 0, 0x34, 0, 0x38, 0, 0x3c].repeat(64);
        store.put_raw(key(0), &half).unwrap();
        store.commit().unwrap();
        let good = root.path().join("good");
        store.compact_copy(&good, 1 << 20).unwrap();
        assert_eq!(
            TileStore::open(&good, float_options)
                .unwrap()
                .get(key(0))
                .unwrap()
                .unwrap(),
            half
        );
        let offset = store.index[&key(0)].offset;
        drop(store);
        let mut file = OpenOptions::new()
            .write(true)
            .open(source_path.join("tiles.store"))
            .unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[99]).unwrap();
        drop(file);
        let mut store = TileStore::open(&source_path, float_options).unwrap();
        let bad = root.path().join("bad");
        assert!(matches!(
            store.compact_copy(&bad, 1 << 20),
            Err(Error::Corrupt("tile checksum"))
        ));
        assert!(!bad.exists());
        assert!(!fs::read_dir(root.path()).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tile-compaction")));
    }
    #[test]
    fn compaction_failure_at_every_publication_boundary_preserves_last_committed_source() {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("source");
        let mut store = source(&origin);
        let original = fs::read(origin.join("tiles.store")).unwrap();
        for phase in [
            CompactionPhase::Started,
            CompactionPhase::BlobCopied,
            CompactionPhase::BlobsSynced,
            CompactionPhase::IndexSynced,
            CompactionPhase::Published,
        ] {
            let destination = root.path().join(format!("{phase:?}"));
            assert!(store
                .compact_copy_with_observer(&destination, 1 << 20, |current| if current == phase {
                    Err(io::Error::other("injected publication failure"))
                } else {
                    Ok(())
                })
                .is_err());
            assert_eq!(fs::read(origin.join("tiles.store")).unwrap(), original);
            if phase == CompactionPhase::Published {
                assert_eq!(
                    TileStore::open(&destination, options())
                        .unwrap()
                        .tile_count(),
                    3
                );
            } else {
                assert!(!destination.exists());
            }
        }
        assert_eq!(store.get(key(0)).unwrap().unwrap(), pixels(4));
    }
    #[test]
    fn compaction_forced_termination_child() {
        let Some(origin) = std::env::var_os("TILE_COMPACTION_KILL_SOURCE") else {
            return;
        };
        let destination = std::env::var_os("TILE_COMPACTION_KILL_DESTINATION").unwrap();
        let phase = std::env::var("TILE_COMPACTION_KILL_PHASE").unwrap();
        let mut store = TileStore::open(origin, options()).unwrap();
        store
            .compact_copy_with_observer(destination, 1 << 20, |current| {
                if format!("{current:?}") == phase {
                    std::process::exit(91);
                }
                Ok(())
            })
            .unwrap();
        panic!("termination phase was not reached");
    }
    #[test]
    fn forced_termination_during_compaction_leaves_source_correct_and_destination_atomic() {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("source");
        drop(source(&origin));
        let original = fs::read(origin.join("tiles.store")).unwrap();
        let index = fs::read(origin.join("tiles.index")).unwrap();
        for phase in [
            CompactionPhase::Started,
            CompactionPhase::BlobCopied,
            CompactionPhase::BlobsSynced,
            CompactionPhase::IndexSynced,
            CompactionPhase::Published,
        ] {
            let destination = root.path().join(format!("crash-{phase:?}"));
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "compact::tests::compaction_forced_termination_child",
                    "--nocapture",
                ])
                .env("TILE_COMPACTION_KILL_SOURCE", &origin)
                .env("TILE_COMPACTION_KILL_DESTINATION", &destination)
                .env("TILE_COMPACTION_KILL_PHASE", format!("{phase:?}"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(91));
            assert_eq!(fs::read(origin.join("tiles.store")).unwrap(), original);
            assert_eq!(fs::read(origin.join("tiles.index")).unwrap(), index);
            let mut reopened = TileStore::open(&origin, options()).unwrap();
            assert_eq!(reopened.get(key(0)).unwrap().unwrap(), pixels(4));
            drop(reopened);
            if phase == CompactionPhase::Published {
                let mut copy = TileStore::open(&destination, options()).unwrap();
                assert_eq!(copy.get(key(2)).unwrap().unwrap(), pixels(6));
            } else {
                assert!(!destination.exists());
            }
        }
    }
}
