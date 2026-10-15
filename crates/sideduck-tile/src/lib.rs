//! Sparse, bounded tile storage with an explicit durable commit boundary.
//!
//! Hot tiles are decoded in a byte-budgeted LRU. Eviction appends compressed
//! blobs, but only [`TileStore::commit`] publishes them through an atomically
//! replaced index. Dropping a store abandons all changes since its last commit.
//! Reads map only one compressed blob at a time, never the whole document.
//!
//! The tile cache ceiling excludes the index, compression scratch (at most one
//! tile's LZ4 maximum size), and one mapped blob. The benchmark independently
//! checks process resident memory against its total ceiling.
//!
//! The document directory is exclusively owned by this writer while open.
//! All cooperating writers must use the store lock; external file modification
//! while open is unsupported because memory mapping requires immutable blobs.

mod persistence;
mod types;

pub use types::{Error, PixelFormat, Result, StoreOptions, StoreStats, TileKey};

use memmap2::MmapOptions;
use persistence::{Record, STORE_MAGIC};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

struct CachedTile {
    bytes: Vec<u8>,
    dirty: bool,
    generation: u64,
    last_used: u64,
    pins: usize,
}

/// An append-only local tile store. The file lock lasts for this object's lifetime.
/// This initial store accepts arbitrary tile addresses; canvas and layer bounds
/// belong to the document model above it.
pub struct TileStore {
    directory: PathBuf,
    file: File,
    options: StoreOptions,
    tile_bytes: usize,
    index: BTreeMap<TileKey, Record>,
    cache: HashMap<TileKey, CachedTile>,
    lru: BTreeSet<(u64, TileKey)>,
    dirty_layers: BTreeSet<u32>,
    dirty_tiles: BTreeSet<TileKey>,
    generation: u64,
    clock: u64,
    stats: StoreStats,
    uncommitted: bool,
}

impl TileStore {
    /// Create an empty store without replacing any existing store files.
    pub fn create(directory: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let tile_bytes = options.validate()?;
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        if directory.join("tiles.index").exists() {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists).into());
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.join("tiles.store"))?;
        persistence::lock(&file)?;
        file.write_all(STORE_MAGIC)?;
        let mut store = Self::new(directory, file, options, tile_bytes, BTreeMap::new(), 0);
        store.commit()?;
        Ok(store)
    }

    /// Open only the last committed index; trailing uncommitted blobs are ignored.
    /// The index, record bounds, and store header are checked eagerly. Blob CRCs
    /// and compressed streams are checked on demand, without scanning cold data.
    pub fn open(directory: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let tile_bytes = options.validate()?;
        let directory = directory.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("tiles.store"))?;
        persistence::lock(&file)?;
        let mut header = [0; 8];
        file.read_exact(&mut header)
            .map_err(|_| Error::Corrupt("truncated store header"))?;
        if &header != STORE_MAGIC {
            return Err(Error::Corrupt("store magic or version"));
        }
        let (index, generation) =
            persistence::read_index(&directory, options, file.metadata()?.len())?;
        Ok(Self::new(
            directory, file, options, tile_bytes, index, generation,
        ))
    }

    fn new(
        directory: PathBuf,
        file: File,
        options: StoreOptions,
        tile_bytes: usize,
        index: BTreeMap<TileKey, Record>,
        generation: u64,
    ) -> Self {
        Self {
            directory,
            file,
            options,
            tile_bytes,
            index,
            cache: HashMap::new(),
            lru: BTreeSet::new(),
            dirty_layers: BTreeSet::new(),
            dirty_tiles: BTreeSet::new(),
            generation,
            clock: 0,
            stats: StoreStats::default(),
            uncommitted: false,
        }
    }

    pub const fn options(&self) -> StoreOptions {
        self.options
    }

    pub const fn tile_bytes(&self) -> usize {
        self.tile_bytes
    }

    /// Current logical tile count, including changes not yet committed.
    pub fn tile_count(&self) -> usize {
        self.index.len()
            + self
                .cache
                .keys()
                .filter(|key| !self.index.contains_key(key))
                .count()
    }

    /// Addresses of all existing tiles, with no duplicate or empty addresses.
    /// The ordering of new, uncommitted cache entries is unspecified.
    pub fn keys(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.index.keys().copied().chain(
            self.cache
                .keys()
                .filter(|key| !self.index.contains_key(key))
                .copied(),
        )
    }

    pub fn contains(&self, key: TileKey) -> bool {
        self.index.contains_key(&key) || self.cache.contains_key(&key)
    }

    /// Layer invalidations accumulate independently of commits until drained.
    pub fn dirty_layers(&self) -> impl Iterator<Item = u32> + '_ {
        self.dirty_layers.iter().copied()
    }

    pub fn drain_dirty_layers(&mut self) -> BTreeSet<u32> {
        std::mem::take(&mut self.dirty_layers)
    }

    /// Tile invalidations, including deleted addresses, since the last drain.
    /// Repeated edits of one address consume only one metadata entry.
    pub fn dirty_tiles(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.dirty_tiles.iter().copied()
    }

    /// Drain tile invalidations independently of layer invalidations and commits.
    /// Consumers should drain each frame so deleted-address metadata is bounded
    /// by the unique addresses changed between frames.
    pub fn drain_dirty_tiles(&mut self) -> BTreeSet<TileKey> {
        std::mem::take(&mut self.dirty_tiles)
    }

    pub const fn has_uncommitted_changes(&self) -> bool {
        self.uncommitted
    }

    pub fn stats(&self) -> StoreStats {
        StoreStats {
            cache_entries: self.cache.len(),
            persisted_tiles: self.index.len(),
            ..self.stats
        }
    }

    /// Replace a tile, or remove it if every alpha is exactly zero. Float +0 and
    /// -0 both count as transparent; tiny nonzero alpha values are preserved.
    /// Explicit replacement or removal is allowed for pinned tiles.
    pub fn put(&mut self, key: TileKey, bytes: &[u8]) -> Result<()> {
        if bytes.len() != self.tile_bytes {
            return Err(Error::TileLength {
                expected: self.tile_bytes,
                actual: bytes.len(),
            });
        }
        if self.options.format.is_transparent(bytes) {
            self.remove(key);
            return Ok(());
        }
        let generation = self.next_generation()?;
        if let Some(tile) = self.cache.get_mut(&key) {
            tile.bytes.copy_from_slice(bytes);
            tile.dirty = true;
            tile.generation = generation;
            self.touch(key);
        } else {
            self.make_room(self.tile_bytes)?;
            self.insert_cache(key, bytes.to_vec(), generation, true);
        }
        self.mark_changed(key);
        Ok(())
    }

    /// Load a tile without materializing empty addresses. The returned borrow
    /// cannot outlive a later cache mutation because it borrows this store.
    pub fn get(&mut self, key: TileKey) -> Result<Option<&[u8]>> {
        if self.cache.contains_key(&key) {
            self.stats.cache_hits += 1;
            self.touch(key);
            return Ok(self.cache.get(&key).map(|tile| tile.bytes.as_slice()));
        }
        let Some(record) = self.index.get(&key).copied() else {
            return Ok(None);
        };
        self.stats.cache_misses += 1;
        self.make_room(self.tile_bytes)?;
        let bytes = self.read_blob(record)?;
        self.insert_cache(key, bytes, record.generation, false);
        Ok(self.cache.get(&key).map(|tile| tile.bytes.as_slice()))
    }

    /// Edit an existing tile in place; returns false for a sparse address.
    /// Changes are tracked even if the edit closure panics. Clearing all alpha
    /// removes the tile and its pins, because explicit edits override pinning.
    pub fn edit(&mut self, key: TileKey, edit: impl FnOnce(&mut [u8])) -> Result<bool> {
        if self.get(key)?.is_none() {
            return Ok(false);
        }
        let generation = self.next_generation()?;
        self.mark_changed(key);
        let tile = self.cache.get_mut(&key).expect("loaded tile");
        tile.dirty = true;
        tile.generation = generation;
        edit(&mut tile.bytes);
        if self.options.format.is_transparent(&tile.bytes) {
            self.remove(key);
        }
        Ok(true)
    }

    /// Explicit deletion overrides pins. This is distinct from eviction, which
    /// never invalidates a pinned tile. Returns whether a tile existed.
    pub fn remove(&mut self, key: TileKey) -> bool {
        let cached = self.remove_cache(key).is_some();
        let persisted = self.index.remove(&key).is_some();
        if cached || persisted {
            self.mark_changed(key);
        }
        cached || persisted
    }

    /// Ensure a tile is resident and increment its pin count. Sparse addresses
    /// return false. Every successful pin should be matched by an unpin.
    pub fn pin(&mut self, key: TileKey) -> Result<bool> {
        if self.get(key)?.is_none() {
            return Ok(false);
        }
        let tile = self.cache.get_mut(&key).expect("loaded tile");
        tile.pins = tile
            .pins
            .checked_add(1)
            .ok_or(Error::InvalidOptions("pin count overflow"))?;
        Ok(true)
    }

    pub fn unpin(&mut self, key: TileKey) -> bool {
        if let Some(tile) = self.cache.get_mut(&key) {
            if tile.pins > 0 {
                tile.pins -= 1;
                return true;
            }
        }
        false
    }

    /// Bounded synchronous prefetch for this gate. Callers choose their viewport
    /// and layer neighborhood; fetching never increases the configured ceiling.
    pub fn prefetch(&mut self, keys: impl IntoIterator<Item = TileKey>) -> Result<usize> {
        let mut loaded = 0;
        for key in keys {
            if self.get(key)?.is_some() {
                loaded += 1;
            }
        }
        Ok(loaded)
    }

    /// Evict all unpinned tiles, flushing dirty blobs first. This does not commit.
    /// If pinned tiles remain, returns AllTilesPinned after flushing other tiles.
    pub fn evict_all(&mut self) -> Result<()> {
        while !self.cache.is_empty() {
            self.evict_one()?;
        }
        Ok(())
    }

    /// Change the payload ceiling. A pinned set larger than the requested budget
    /// rejects the change; the prior budget remains in force.
    pub fn set_cache_budget(&mut self, bytes: usize) -> Result<()> {
        let updated = StoreOptions {
            cache_budget_bytes: bytes,
            ..self.options
        };
        updated.validate()?;
        let pinned = self.cache.values().filter(|tile| tile.pins > 0).count() * self.tile_bytes;
        if pinned > bytes {
            return Err(Error::AllTilesPinned);
        }
        while self.stats.resident_bytes > bytes {
            self.evict_one()?;
        }
        self.options = updated;
        Ok(())
    }

    /// Durably publish all edits: append dirty blobs, sync the store, sync a new
    /// checksummed index, atomically rename it, then sync the directory. A process
    /// killed before the rename sees the previous complete commit on reopening;
    /// after rename it sees the new complete commit. Orphan appended blobs are
    /// harmless and are not reclaimed by this initial implementation.
    pub fn commit(&mut self) -> Result<()> {
        let dirty: Vec<_> = self
            .cache
            .iter()
            .filter_map(|(key, tile)| tile.dirty.then_some(*key))
            .collect();
        for key in dirty {
            self.flush_tile(key)?;
        }
        self.file.sync_all()?;
        persistence::write_index(&self.directory, self.options, &self.index, self.generation)?;
        self.uncommitted = false;
        Ok(())
    }

    fn mark_changed(&mut self, key: TileKey) {
        self.dirty_layers.insert(key.layer);
        self.dirty_tiles.insert(key);
        self.uncommitted = true;
    }

    fn next_generation(&mut self) -> Result<u64> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(Error::InvalidOptions("generation overflow"))?;
        Ok(self.generation)
    }

    fn touch(&mut self, key: TileKey) {
        // Renumbering is fantastically rare, but avoids wrapping LRU timestamps.
        if self.clock == u64::MAX {
            let ordered: Vec<_> = self.lru.iter().map(|(_, key)| *key).collect();
            self.lru.clear();
            for (tick, key) in ordered.into_iter().enumerate() {
                let tick = tick as u64;
                self.cache.get_mut(&key).expect("LRU entry").last_used = tick;
                self.lru.insert((tick, key));
            }
            self.clock = self.cache.len() as u64;
        }
        self.clock += 1;
        let tile = self.cache.get_mut(&key).expect("cached tile");
        self.lru.remove(&(tile.last_used, key));
        tile.last_used = self.clock;
        self.lru.insert((self.clock, key));
    }

    fn insert_cache(&mut self, key: TileKey, bytes: Vec<u8>, generation: u64, dirty: bool) {
        self.cache.insert(
            key,
            CachedTile {
                bytes,
                dirty,
                generation,
                last_used: 0,
                pins: 0,
            },
        );
        self.stats.resident_bytes += self.tile_bytes;
        self.stats.peak_resident_bytes = self
            .stats
            .peak_resident_bytes
            .max(self.stats.resident_bytes);
        self.touch(key);
        debug_assert!(self.stats.resident_bytes <= self.options.cache_budget_bytes);
    }

    fn remove_cache(&mut self, key: TileKey) -> Option<CachedTile> {
        let tile = self.cache.remove(&key)?;
        self.lru.remove(&(tile.last_used, key));
        self.stats.resident_bytes -= self.tile_bytes;
        Some(tile)
    }

    fn make_room(&mut self, bytes: usize) -> Result<()> {
        while self.stats.resident_bytes > self.options.cache_budget_bytes - bytes {
            self.evict_one()?;
        }
        Ok(())
    }

    fn evict_one(&mut self) -> Result<()> {
        let key = self
            .lru
            .iter()
            .find_map(|(_, key)| {
                (self.cache.get(key).expect("LRU entry").pins == 0).then_some(*key)
            })
            .ok_or(Error::AllTilesPinned)?;
        // Failure leaves the tile resident and dirty so the caller may retry.
        self.flush_tile(key)?;
        self.remove_cache(key);
        self.stats.evictions += 1;
        Ok(())
    }

    fn flush_tile(&mut self, key: TileKey) -> Result<()> {
        let tile = self.cache.get(&key).expect("cached tile");
        if !tile.dirty {
            return Ok(());
        }
        // Also handles a caller catching a panic in an edit that cleared alpha.
        if self.options.format.is_transparent(&tile.bytes) {
            self.index.remove(&key);
            self.remove_cache(key);
            return Ok(());
        }
        let compressed = lz4_flex::block::compress(&tile.bytes);
        let (codec, blob) = if compressed.len() < tile.bytes.len() {
            (1, compressed.as_slice())
        } else {
            (0, tile.bytes.as_slice())
        };
        let offset = self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(blob)?;
        let record = Record {
            generation: tile.generation,
            offset,
            length: blob.len() as u32,
            checksum: crc32fast::hash(&tile.bytes),
            codec,
        };
        self.stats.compressed_bytes_written += blob.len() as u64;
        self.index.insert(key, record);
        self.cache.get_mut(&key).expect("cached tile").dirty = false;
        Ok(())
    }

    fn read_blob(&mut self, record: Record) -> Result<Vec<u8>> {
        // SAFETY: this object holds the store's exclusive advisory lock. Its only
        // writes append beyond existing blobs, never mutate/truncate them. Maps
        // are local to this function and are dropped before any later append.
        // External modification ignoring the lock is explicitly unsupported.
        let mapping = unsafe {
            MmapOptions::new()
                .offset(record.offset)
                .len(record.length as usize)
                .map(&self.file)?
        };
        let mut bytes = vec![0; self.tile_bytes];
        match record.codec {
            0 => bytes.copy_from_slice(&mapping),
            1 => {
                let decoded = lz4_flex::block::decompress_into(&mapping, &mut bytes)
                    .map_err(|_| Error::Corrupt("compressed tile"))?;
                if decoded != self.tile_bytes {
                    return Err(Error::Corrupt("decoded tile length"));
                }
            }
            _ => return Err(Error::Corrupt("tile codec")),
        }
        if crc32fast::hash(&bytes) != record.checksum {
            return Err(Error::Corrupt("tile checksum"));
        }
        self.stats.compressed_bytes_read += u64::from(record.length);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;
