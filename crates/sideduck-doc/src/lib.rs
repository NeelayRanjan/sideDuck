//! Local document packages with an authoritative content-addressed commit log.
//! A complete log frame references an immutable, synced tile index snapshot.
//! Recovery restores that snapshot, even if a later tile-only commit completed.

mod model;
pub mod selection;
mod storage;
pub mod transform;

pub use model::{
    ContentId, DocumentState, Layer, LayerId, LayerKind, Manifest, Operation, StrokeRecord,
    TileAddress, TileEdit, TileFormat, FORMAT_VERSION,
};
use model::{History, HistoryEntry, TileChange};
use sideduck_tile::{StoreOptions, TileStore};
use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use storage::{LocatedRecord, Record};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    Tile(sideduck_tile::Error),
    Invalid(&'static str),
    Corrupt(&'static str),
    Limit(&'static str),
    AlreadyOpen,
    NoUndo,
    NoRedo,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
            Self::Tile(error) => error.fmt(f),
            Self::Invalid(reason) => write!(f, "invalid document operation: {reason}"),
            Self::Corrupt(reason) => write!(f, "invalid document data: {reason}"),
            Self::Limit(reason) => write!(f, "document limit: {reason}"),
            Self::AlreadyOpen => f.write_str("document is already open for writing"),
            Self::NoUndo => f.write_str("no retained operation to undo"),
            Self::NoRedo => f.write_str("no retained operation to redo"),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<sideduck_tile::Error> for Error {
    fn from(error: sideduck_tile::Error) -> Self {
        Self::Tile(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DocumentOptions {
    pub tile_cache_bytes: usize,
    /// Retained undo pre/post-image payloads plus operation/state metadata.
    pub undo_budget_bytes: u64,
    pub checkpoint_interval: u64,
    pub max_record_bytes: usize,
}
impl Default for DocumentOptions {
    fn default() -> Self {
        Self {
            tile_cache_bytes: 64 * 1024 * 1024,
            undo_budget_bytes: 512 * 1024 * 1024,
            checkpoint_interval: 32,
            max_record_bytes: 16 * 1024 * 1024,
        }
    }
}
impl DocumentOptions {
    fn validate(self, manifest: &Manifest) -> Result<()> {
        manifest.validate()?;
        if self.tile_cache_bytes < manifest.tile_bytes()
            || self.checkpoint_interval == 0
            || !(1024..=64 * 1024 * 1024).contains(&self.max_record_bytes)
        {
            return Err(Error::Invalid(
                "cache, checkpoint interval, or record bound is invalid",
            ));
        }
        Ok(())
    }
    fn store(self, manifest: &Manifest) -> StoreOptions {
        StoreOptions {
            tile_size: manifest.tile_size,
            format: manifest.tile_format.into(),
            cache_budget_bytes: self.tile_cache_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitStage {
    TileWritten,
    TilesCommitted,
    SnapshotDurable,
    BeforeLogAppend,
    LogDurable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub sequence: u64,
    pub content_id: ContentId,
    pub checkpoint: bool,
}
pub struct ReplayResult {
    pub state: DocumentState,
    pub tiles: TileStore,
    pub replayed_operations: u64,
}

pub struct Document {
    directory: PathBuf,
    _lock: File,
    log: File,
    manifest: Manifest,
    options: DocumentOptions,
    tiles: Option<TileStore>,
    latest: LocatedRecord,
    checkpoints: VecDeque<u64>,
}

impl Document {
    pub fn create(
        path: impl AsRef<Path>,
        manifest: Manifest,
        options: DocumentOptions,
    ) -> Result<Self> {
        options.validate(&manifest)?;
        let directory = path.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        let lock = document_lock(&directory)?;
        if directory.join("manifest.json").exists() || directory.join("ops.log").exists() {
            return Err(Error::Io(std::io::ErrorKind::AlreadyExists.into()));
        }
        fs::create_dir_all(directory.join("checkpoints"))?;
        fs::create_dir_all(directory.join("objects"))?;
        let tiles = TileStore::create(&directory, options.store(&manifest))?;
        storage::atomic_write(
            &directory.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        storage::atomic_write(
            &directory.join("thumbnail.png"),
            &encode_thumbnail(1, 1, &[0; 4])?,
        )?;
        let index_hash = storage::copy_durable(
            &directory.join("tiles.index"),
            &storage::index_path(&directory, 0),
        )?;
        let mut log = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.join("ops.log"))?;
        log.write_all(storage::LOG_MAGIC)?;
        let operation = Operation::Initialize;
        let record = Record {
            version: 1,
            sequence: 0,
            previous: [0; 32],
            content_id: storage::content_id(&operation, &[])?,
            operation,
            state: DocumentState::default(),
            changes: Vec::new(),
            index_hash,
            checkpoint: true,
            history: History::default(),
        };
        let latest = storage::append(&mut log, &record, options.max_record_bytes)?;
        storage::sync_directory(&directory)?;
        Ok(Self {
            directory,
            _lock: lock,
            log,
            manifest,
            options,
            tiles: Some(tiles),
            latest,
            checkpoints: VecDeque::from([0]),
        })
    }

    pub fn open(path: impl AsRef<Path>, options: DocumentOptions) -> Result<Self> {
        let directory = path.as_ref().to_path_buf();
        let lock = document_lock(&directory)?;
        let manifest: Manifest = serde_json::from_slice(&storage::read_bounded(
            &directory.join("manifest.json"),
            64 * 1024,
        )?)?;
        options.validate(&manifest)?;
        let mut checkpoints = VecDeque::new();
        let latest = storage::scan(
            &directory.join("ops.log"),
            options.max_record_bytes,
            |record| {
                if record.record.checkpoint {
                    remember_checkpoint(&mut checkpoints, record.record.sequence);
                }
                Ok(())
            },
        )?;
        restore_index(&directory, &latest)?;
        let mut log = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("ops.log"))?;
        log.set_len(latest.end)?;
        log.seek(SeekFrom::End(0))?;
        log.sync_all()?;
        let tiles = TileStore::open(&directory, options.store(&manifest))?;
        Ok(Self {
            directory,
            _lock: lock,
            log,
            manifest,
            options,
            tiles: Some(tiles),
            latest,
            checkpoints,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn state(&self) -> &DocumentState {
        &self.latest.record.state
    }
    pub fn sequence(&self) -> u64 {
        self.latest.record.sequence
    }
    pub fn path(&self) -> &Path {
        &self.directory
    }

    /// Preserve a source image/PDF without interpreting or resizing it. The
    /// following reference-layer operation gives this immutable object meaning.
    pub fn put_source(&self, bytes: &[u8]) -> Result<ContentId> {
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(Error::Limit("reference source exceeds 64 MiB"));
        }
        let id = storage::hash(bytes);
        let path = self
            .directory
            .join("objects")
            .join(format!("{}.source", storage::hex(id)));
        if path.exists() {
            if self.read_source(id)? != bytes {
                return Err(Error::Corrupt("reference source content address collision"));
            }
        } else {
            storage::atomic_write(&path, bytes)?;
        }
        Ok(id)
    }

    pub fn read_source(&self, id: ContentId) -> Result<Vec<u8>> {
        let path = self
            .directory
            .join("objects")
            .join(format!("{}.source", storage::hex(id)));
        let bytes = storage::read_bounded(&path, 64 * 1024 * 1024)?;
        if storage::hash(&bytes) != id {
            return Err(Error::Corrupt("reference source content checksum"));
        }
        Ok(bytes)
    }

    /// Replace a derived preview atomically; this does not change document undo.
    pub fn set_thumbnail_rgba(&self, width: u32, height: u32, pixels: &[u8]) -> Result<()> {
        storage::atomic_write(
            &self.directory.join("thumbnail.png"),
            &encode_thumbnail(width, height, pixels)?,
        )
    }
    pub fn can_undo(&self) -> bool {
        !self.latest.record.history.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.latest.record.history.redo.is_empty()
    }
    pub fn next_layer_id(&self) -> Result<LayerId> {
        u32::try_from(self.state().next_layer_id)
            .map_err(|_| Error::Limit("layer identifier space exhausted"))
    }
    pub fn undo_retained_bytes(&self) -> u64 {
        self.latest
            .record
            .history
            .undo
            .iter()
            .chain(&self.latest.record.history.redo)
            .map(|entry| entry.bytes)
            .sum()
    }
    pub fn available_checkpoints(&self) -> Vec<u64> {
        self.checkpoints.iter().copied().collect()
    }
    pub fn tile_stats(&self) -> sideduck_tile::StoreStats {
        self.tiles.as_ref().expect("open tile store").stats()
    }
    pub fn tile_keys(&self) -> impl Iterator<Item = TileAddress> + '_ {
        self.tiles
            .as_ref()
            .expect("open tile store")
            .keys()
            .map(TileAddress::from)
    }
    pub fn read_tile(&mut self, address: TileAddress) -> Result<Option<&[u8]>> {
        self.validate_address(address)?;
        Ok(self
            .tiles
            .as_mut()
            .expect("open tile store")
            .get(address.into())?)
    }

    pub fn apply(
        &mut self,
        operation: Operation,
        edits: impl IntoIterator<Item = TileEdit>,
    ) -> Result<Commit> {
        self.apply_with_observer(operation, edits, &mut |_| Ok(()))
    }

    /// Fault-injection/progress observer. Every failure resolves to a complete
    /// durable commit before returning; forced process exits bypass cleanup.
    pub fn apply_with_observer(
        &mut self,
        operation: Operation,
        edits: impl IntoIterator<Item = TileEdit>,
        observer: &mut impl FnMut(CommitStage) -> Result<()>,
    ) -> Result<Commit> {
        let result = self.apply_inner(operation, edits, observer);
        if result.is_err() {
            self.recover()?;
        }
        result
    }

    fn apply_inner(
        &mut self,
        operation: Operation,
        edits: impl IntoIterator<Item = TileEdit>,
        observer: &mut impl FnMut(CommitStage) -> Result<()>,
    ) -> Result<Commit> {
        let sequence = self
            .sequence()
            .checked_add(1)
            .ok_or(Error::Limit("operation sequence exhausted"))?;
        let mut state = self.state().clone();
        let removed = state.apply(&operation, sequence)?;
        let mut changes = Vec::new();
        let mut seen = BTreeSet::new();
        for edit in edits {
            self.validate_address(edit.address)?;
            if !operation.permits_tile(edit.address.layer) || !seen.insert(edit.address) {
                return Err(Error::Invalid(
                    "tile edit is duplicated or outside its operation",
                ));
            }
            state.ensure_editable(edit.address.layer)?;
            self.change_tile(edit, &mut changes)?;
            observer(CommitStage::TileWritten)?;
            if changes.len().saturating_mul(180) > self.options.max_record_bytes {
                return Err(Error::Limit(
                    "too many dirty tile references in one operation",
                ));
            }
        }
        let deleted: Vec<_> = self
            .tiles
            .as_ref()
            .expect("open tile store")
            .keys()
            .filter(|key| removed.contains(&key.layer))
            .collect();
        for key in deleted {
            self.change_tile(
                TileEdit {
                    address: key.into(),
                    pixels: None,
                },
                &mut changes,
            )?;
            observer(CommitStage::TileWritten)?;
            if changes.len().saturating_mul(180) > self.options.max_record_bytes {
                return Err(Error::Limit("too many removed tiles in one operation"));
            }
        }
        for change in &changes {
            if let Some(layer) = state
                .layers
                .iter_mut()
                .find(|layer| layer.id == change.address.layer)
            {
                layer.revision = sequence;
            }
        }
        let mut history = self.latest.record.history.clone();
        history.redo.clear();
        let image_bytes = changes.len() as u64 * self.manifest.tile_bytes() as u64 * 2;
        let metadata_bytes = storage::json_bytes(
            &(&operation, &changes, self.state(), &state),
            self.options.max_record_bytes,
        )?
        .len() as u64;
        history.undo.push(HistoryEntry {
            sequence,
            bytes: image_bytes.saturating_add(metadata_bytes),
        });
        history.trim(self.options.undo_budget_bytes);
        let checkpoint =
            operation.expensive() || sequence.is_multiple_of(self.options.checkpoint_interval);
        self.publish(operation, state, changes, history, checkpoint, observer)
    }

    fn change_tile(&mut self, edit: TileEdit, changes: &mut Vec<TileChange>) -> Result<()> {
        if let Some(bytes) = &edit.pixels {
            if bytes.len() != self.manifest.tile_bytes() {
                return Err(Error::Invalid(
                    "tile edit has an incorrect pixel byte length",
                ));
            }
        }
        let store = self.tiles.as_mut().expect("open tile store");
        let before = match store.get(edit.address.into())? {
            Some(bytes) => Some(storage::put_object(&self.directory, bytes)?),
            None => None,
        };
        let after = match &edit.pixels {
            Some(bytes) if !transparent(self.manifest.tile_format, bytes) => {
                Some(storage::put_object(&self.directory, bytes)?)
            }
            _ => None,
        };
        if before == after {
            return Ok(());
        }
        if let Some(bytes) = &edit.pixels {
            store.put(edit.address.into(), bytes)?;
        } else {
            store.remove(edit.address.into());
        }
        changes.push(TileChange {
            address: edit.address,
            before,
            after,
        });
        Ok(())
    }

    fn publish(
        &mut self,
        operation: Operation,
        state: DocumentState,
        changes: Vec<TileChange>,
        history: History,
        checkpoint: bool,
        observer: &mut impl FnMut(CommitStage) -> Result<()>,
    ) -> Result<Commit> {
        let sequence = self
            .sequence()
            .checked_add(1)
            .ok_or(Error::Limit("operation sequence exhausted"))?;
        let content_id = storage::content_id(&operation, &changes)?;
        let mut record = Record {
            version: 1,
            sequence,
            previous: self.latest.hash,
            content_id,
            operation,
            state,
            changes,
            index_hash: [0; 32],
            checkpoint,
            history,
        };
        storage::json_bytes(&record, self.options.max_record_bytes)?;
        self.tiles.as_mut().expect("open tile store").commit()?;
        observer(CommitStage::TilesCommitted)?;
        record.index_hash = storage::copy_durable(
            &self.directory.join("tiles.index"),
            &storage::index_path(&self.directory, sequence),
        )?;
        observer(CommitStage::SnapshotDurable)?;
        observer(CommitStage::BeforeLogAppend)?;
        self.latest = storage::append(&mut self.log, &record, self.options.max_record_bytes)?;
        if checkpoint {
            remember_checkpoint(&mut self.checkpoints, sequence);
        }
        observer(CommitStage::LogDurable)?;
        self.cleanup_indices()?;
        Ok(Commit {
            sequence,
            content_id,
            checkpoint,
        })
    }

    pub fn undo(&mut self) -> Result<Commit> {
        self.move_history(false)
    }
    pub fn redo(&mut self) -> Result<Commit> {
        self.move_history(true)
    }
    fn move_history(&mut self, redo: bool) -> Result<Commit> {
        let result = self.move_history_inner(redo);
        if result.is_err() {
            self.recover()?;
        }
        result
    }
    fn move_history_inner(&mut self, redo: bool) -> Result<Commit> {
        let mut history = self.latest.record.history.clone();
        let entry = if redo {
            history.redo.pop().ok_or(Error::NoRedo)?
        } else {
            history.undo.pop().ok_or(Error::NoUndo)?
        };
        let target = storage::find(
            &self.directory.join("ops.log"),
            self.options.max_record_bytes,
            entry.sequence,
        )?;
        let mut state = if redo {
            target.record.state.clone()
        } else {
            storage::find(
                &self.directory.join("ops.log"),
                self.options.max_record_bytes,
                entry.sequence - 1,
            )?
            .record
            .state
        };
        state.next_layer_id = state.next_layer_id.max(self.state().next_layer_id);
        let sequence = self.sequence() + 1;
        for layer in &mut state.layers {
            layer.revision = sequence;
        }
        let mut changes = Vec::new();
        for change in &target.record.changes {
            let desired = if redo { change.after } else { change.before };
            let pixels = desired
                .map(|id| storage::read_object(&self.directory, id, self.manifest.tile_bytes()))
                .transpose()?;
            self.change_tile(
                TileEdit {
                    address: change.address,
                    pixels,
                },
                &mut changes,
            )?;
        }
        if redo {
            history.undo.push(entry.clone());
        } else {
            history.redo.push(entry.clone());
        }
        history.trim(self.options.undo_budget_bytes);
        let operation = if redo {
            Operation::Redo {
                target: entry.sequence,
            }
        } else {
            Operation::Undo {
                target: entry.sequence,
            }
        };
        self.publish(operation, state, changes, history, false, &mut |_| Ok(()))
    }

    /// Make the current state a durable replay checkpoint without a new edit.
    pub fn checkpoint(&mut self) -> Result<Commit> {
        let result = self.publish(
            Operation::SetActiveLayer {
                id: self.state().active_layer,
            },
            self.state().clone(),
            Vec::new(),
            self.latest.record.history.clone(),
            true,
            &mut |_| Ok(()),
        );
        if result.is_err() {
            self.recover()?;
        }
        result
    }

    /// Reconstruct pixels from a retained checkpoint and subsequent exact
    /// post-images. The blob file copy streams; replay holds one tile at a time.
    pub fn replay_tiles_from_checkpoint(
        &self,
        checkpoint: u64,
        destination: impl AsRef<Path>,
    ) -> Result<ReplayResult> {
        if !self.checkpoints.contains(&checkpoint) {
            return Err(Error::Invalid("checkpoint is not retained"));
        }
        let initial = storage::find(
            &self.directory.join("ops.log"),
            self.options.max_record_bytes,
            checkpoint,
        )?;
        if !initial.record.checkpoint {
            return Err(Error::Invalid("sequence is not a checkpoint"));
        }
        let destination = destination.as_ref();
        if destination.exists() {
            return Err(Error::Io(std::io::ErrorKind::AlreadyExists.into()));
        }
        fs::create_dir_all(destination)?;
        storage::copy_durable(
            &self.directory.join("tiles.store"),
            &destination.join("tiles.store"),
        )?;
        let digest = storage::copy_durable(
            &storage::index_path(&self.directory, checkpoint),
            &destination.join("tiles.index"),
        )?;
        if digest != initial.record.index_hash {
            return Err(Error::Corrupt("checkpoint tile index checksum"));
        }
        let mut tiles = TileStore::open(destination, self.options.store(&self.manifest))?;
        let mut state = initial.record.state;
        let mut replayed_operations = 0;
        storage::scan(
            &self.directory.join("ops.log"),
            self.options.max_record_bytes,
            |record| {
                if record.record.sequence <= checkpoint {
                    return Ok(());
                }
                for change in &record.record.changes {
                    if let Some(id) = change.after {
                        let bytes =
                            storage::read_object(&self.directory, id, self.manifest.tile_bytes())?;
                        tiles.put(change.address.into(), &bytes)?;
                    } else {
                        tiles.remove(change.address.into());
                    }
                }
                state = record.record.state.clone();
                replayed_operations += 1;
                Ok(())
            },
        )?;
        tiles.commit()?;
        Ok(ReplayResult {
            state,
            tiles,
            replayed_operations,
        })
    }

    pub fn operation_ids(&self) -> Result<Vec<ContentId>> {
        let mut ids = Vec::new();
        storage::scan(
            &self.directory.join("ops.log"),
            self.options.max_record_bytes,
            |record| {
                ids.push(record.record.content_id);
                Ok(())
            },
        )?;
        Ok(ids)
    }

    /// Read one verified operation without materializing the complete history.
    pub fn operation(&self, sequence: u64) -> Result<Operation> {
        Ok(storage::find(
            &self.directory.join("ops.log"),
            self.options.max_record_bytes,
            sequence,
        )?
        .record
        .operation)
    }

    fn validate_address(&self, address: TileAddress) -> Result<()> {
        if address.x >= self.manifest.canvas[0].div_ceil(self.manifest.tile_size)
            || address.y >= self.manifest.canvas[1].div_ceil(self.manifest.tile_size)
        {
            return Err(Error::Invalid("tile coordinate lies outside the canvas"));
        }
        Ok(())
    }

    fn recover(&mut self) -> Result<()> {
        self.tiles.take();
        let mut checkpoints = VecDeque::new();
        let latest = storage::scan(
            &self.directory.join("ops.log"),
            self.options.max_record_bytes,
            |record| {
                if record.record.checkpoint {
                    remember_checkpoint(&mut checkpoints, record.record.sequence);
                }
                Ok(())
            },
        )?;
        restore_index(&self.directory, &latest)?;
        self.log.set_len(latest.end)?;
        self.log.seek(SeekFrom::End(0))?;
        self.log.sync_all()?;
        self.tiles = Some(TileStore::open(
            &self.directory,
            self.options.store(&self.manifest),
        )?);
        self.latest = latest;
        self.checkpoints = checkpoints;
        Ok(())
    }

    fn cleanup_indices(&self) -> Result<()> {
        for entry in fs::read_dir(self.directory.join("checkpoints"))? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "index")
            {
                if let Some(sequence) = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.parse::<u64>().ok())
                {
                    if sequence != self.sequence() && !self.checkpoints.contains(&sequence) {
                        fs::remove_file(path)?;
                    }
                }
            }
        }
        storage::sync_directory(&self.directory.join("checkpoints"))
    }
}

fn document_lock(directory: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("document.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::AlreadyOpen),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

fn remember_checkpoint(checkpoints: &mut VecDeque<u64>, sequence: u64) {
    checkpoints.push_back(sequence);
    while checkpoints.len() > 2 {
        checkpoints.pop_front();
    }
}

fn restore_index(directory: &Path, latest: &LocatedRecord) -> Result<()> {
    let snapshot = storage::index_path(directory, latest.record.sequence);
    if storage::hash_reader(&mut File::open(&snapshot)?)? != latest.record.index_hash {
        return Err(Error::Corrupt("committed tile index checksum"));
    }
    storage::copy_durable(&snapshot, &directory.join("tiles.index"))?;
    Ok(())
}

fn transparent(format: TileFormat, bytes: &[u8]) -> bool {
    match format {
        TileFormat::Rgba8 => bytes.chunks_exact(4).all(|pixel| pixel[3] == 0),
        TileFormat::Rgba16Float => bytes
            .chunks_exact(8)
            .all(|pixel| u16::from_le_bytes([pixel[6], pixel[7]]) & 0x7fff == 0),
    }
}

fn encode_thumbnail(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    if width == 0
        || height == 0
        || width > 1024
        || height > 1024
        || pixels.len() != width as usize * height as usize * 4
    {
        return Err(Error::Invalid(
            "thumbnail must contain bounded RGBA8 pixels",
        ));
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
        writer
            .write_image_data(pixels)
            .map_err(std::io::Error::other)?;
    }
    Ok(bytes)
}

impl Drop for Document {
    fn drop(&mut self) {
        // Release the tile writer before the outer package lock.
        self.tiles.take();
    }
}

#[cfg(test)]
mod tests;
