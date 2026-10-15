//! Small parallel reads that transfer decoded allocations directly to the caller.

use super::{read_record, Error, Result, TileKey, TileStore};
use std::io;
use std::thread;

const MAX_KEYS: usize = 32;
const MAX_WORKERS: usize = 8;
const WORKER_STACK_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct ReadCounts {
    misses: u64,
    compressed_bytes: u64,
}

impl TileStore {
    /// Read at most 32 keys, preserving input order and sparse `None` entries.
    /// The caller owns the returned pixels; they are not inserted in the cache.
    /// Cached dirty values take precedence over committed blobs.
    ///
    /// `keys.len() * tile_bytes()` must fit `max_output_bytes`, even when some
    /// keys are empty. The additional working set is bounded metadata for 32
    /// entries, one compressed mapping per worker (at most one tile), and an
    /// explicit 2 MiB stack per worker. The existing decoded cache is unchanged
    /// except for hit/LRU accounting. Workers must be between one and eight;
    /// fewer threads are started when a chunk has no persisted misses.
    ///
    /// All workers join before this call returns, including failure paths. The
    /// store's exclusive lock and mutable borrow prevent writes while its
    /// immutable records and file are borrowed by the scoped workers.
    pub fn read_batch_owned(
        &mut self,
        keys: &[TileKey],
        max_output_bytes: usize,
        workers: usize,
    ) -> Result<Vec<Option<Vec<u8>>>> {
        if !(1..=MAX_WORKERS).contains(&workers) || keys.len() > MAX_KEYS {
            return Err(Error::InvalidOptions(
                "tile batches require 1 through 8 workers and at most 32 keys",
            ));
        }
        let required = keys
            .len()
            .checked_mul(self.tile_bytes)
            .ok_or(Error::InvalidOptions(
                "tile batch output byte count overflow",
            ))?;
        if required > max_output_bytes {
            return Err(Error::BudgetTooSmall {
                required,
                budget: max_output_bytes,
            });
        }
        let mut output = Vec::with_capacity(keys.len());
        let mut records = Vec::with_capacity(keys.len());
        for &key in keys {
            if let Some(tile) = self.cache.get(&key) {
                output.push(Some(tile.bytes.clone()));
                records.push(None);
                self.stats.cache_hits += 1;
                self.touch(key);
            } else {
                output.push(None);
                records.push(self.index.get(&key).copied());
            }
        }
        if records.iter().all(Option::is_none) {
            return Ok(output);
        }
        let chunk_size = keys.len().div_ceil(workers);
        let tile_bytes = self.tile_bytes;
        let file = &self.file;
        let (counts, error) = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            let mut error = None;
            for (slots, records) in output
                .chunks_mut(chunk_size)
                .zip(records.chunks(chunk_size))
            {
                if records.iter().all(Option::is_none) {
                    continue;
                }
                let handle = thread::Builder::new()
                    .name("tile batch read".into())
                    .stack_size(WORKER_STACK_BYTES)
                    .spawn_scoped(scope, move || {
                        let mut counts = ReadCounts::default();
                        for (slot, record) in slots.iter_mut().zip(records) {
                            let Some(record) = *record else {
                                continue;
                            };
                            counts.misses += 1;
                            match read_record(file, record, tile_bytes) {
                                Ok(bytes) => {
                                    counts.compressed_bytes += u64::from(record.length);
                                    *slot = Some(bytes);
                                }
                                Err(error) => return (counts, Some(error)),
                            }
                        }
                        (counts, None)
                    });
                match handle {
                    Ok(handle) => handles.push(handle),
                    Err(cause) => {
                        error = Some(Error::Io(cause));
                        break;
                    }
                }
            }
            let mut counts = ReadCounts::default();
            // Join every worker even after another worker fails. Successful
            // reads count toward I/O statistics but no partial batch escapes.
            for handle in handles {
                match handle.join() {
                    Ok((read, cause)) => {
                        counts.misses += read.misses;
                        counts.compressed_bytes += read.compressed_bytes;
                        if error.is_none() {
                            error = cause;
                        }
                    }
                    Err(_) => {
                        if error.is_none() {
                            error = Some(Error::Io(io::Error::other("tile batch worker panicked")));
                        }
                    }
                }
            }
            (counts, error)
        });
        self.stats.cache_misses += counts.misses;
        self.stats.compressed_bytes_read += counts.compressed_bytes;
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PixelFormat, StoreOptions};
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};

    fn options(format: PixelFormat) -> StoreOptions {
        StoreOptions {
            tile_size: 8,
            format,
            cache_budget_bytes: 8 * 8 * format.bytes_per_pixel(),
        }
    }

    fn key(layer: u32) -> TileKey {
        TileKey { layer, x: 0, y: 0 }
    }

    fn pixels(layer: u32, format: PixelFormat) -> Vec<u8> {
        match format {
            PixelFormat::Rgba8 => [layer as u8, 30, 80, 255].repeat(64),
            PixelFormat::Rgba16Float => {
                // Exact finite half-float values, with opaque alpha.
                [layer as u8, 0x30, 0, 0x34, 0, 0x38, 0, 0x3c].repeat(64)
            }
        }
    }

    #[test]
    fn parallel_batch_preserves_dirty_cached_values_sparse_order_and_duplicate_keys() {
        let directory = tempfile::tempdir().unwrap();
        let format = PixelFormat::Rgba8;
        let mut store = TileStore::create(directory.path(), options(format)).unwrap();
        for layer in 0..3 {
            store.put(key(layer), &pixels(layer, format)).unwrap();
        }
        store.commit().unwrap();
        store.edit(key(2), |bytes| bytes[0] = 99).unwrap();
        let before = store.stats();
        let record = store.index[&key(0)];
        let result = store
            .read_batch_owned(&[key(2), key(0), key(100), key(0)], 4 * store.tile_bytes, 2)
            .unwrap();
        let mut dirty = pixels(2, format);
        dirty[0] = 99;
        assert_eq!(
            result,
            vec![
                Some(dirty),
                Some(pixels(0, format)),
                None,
                Some(pixels(0, format))
            ]
        );
        let after = store.stats();
        assert_eq!(after.cache_hits - before.cache_hits, 1);
        assert_eq!(after.cache_misses - before.cache_misses, 2);
        assert_eq!(
            after.compressed_bytes_read - before.compressed_bytes_read,
            2 * u64::from(record.length)
        );
        assert_eq!(after.resident_bytes, before.resident_bytes);
        assert_eq!(after.cache_entries, before.cache_entries);
        assert_eq!(after.evictions, before.evictions);
        assert!(store.cache[&key(2)].dirty);
        assert!(store.has_uncommitted_changes());
    }

    #[test]
    fn parallel_batch_raw_and_compressed_tiles_match_sequential_reads_under_output_ceiling() {
        for format in [PixelFormat::Rgba8, PixelFormat::Rgba16Float] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = TileStore::create(directory.path(), options(format)).unwrap();
            for layer in 0..32 {
                if layer % 2 == 0 {
                    store.put_raw(key(layer), &pixels(layer, format)).unwrap();
                } else {
                    store.put(key(layer), &pixels(layer, format)).unwrap();
                }
            }
            store.commit().unwrap();
            drop(store);
            let mut store = TileStore::open(directory.path(), options(format)).unwrap();
            let keys: Vec<_> = (0..32).rev().map(key).collect();
            let ceiling = keys.len() * store.tile_bytes;
            for workers in [1, 3, 8] {
                let result = store.read_batch_owned(&keys, ceiling, workers).unwrap();
                let allocated: usize = result.iter().flatten().map(Vec::capacity).sum();
                assert_eq!(allocated, ceiling);
                for (&key, actual) in keys.iter().zip(result) {
                    assert_eq!(actual.unwrap(), pixels(key.layer, format));
                }
                assert_eq!(store.stats().resident_bytes, 0);
                assert_eq!(store.stats().cache_entries, 0);
            }
            assert_eq!(store.stats().cache_misses, 96);
            assert_eq!(store.stats().cache_hits, 0);
            for key in keys {
                assert_eq!(store.get(key).unwrap().unwrap(), pixels(key.layer, format));
            }
        }
    }

    #[test]
    fn parallel_batch_invalid_limits_fail_before_reads_or_cache_changes() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = TileStore::create(directory.path(), options(PixelFormat::Rgba8)).unwrap();
        store.put(key(1), &pixels(1, PixelFormat::Rgba8)).unwrap();
        assert!(matches!(
            store.read_batch_owned(&[key(1)], usize::MAX, 0),
            Err(Error::InvalidOptions(_))
        ));
        assert!(matches!(
            store.read_batch_owned(&[key(1)], usize::MAX, 9),
            Err(Error::InvalidOptions(_))
        ));
        assert!(matches!(
            store.read_batch_owned(&[key(1); 33], usize::MAX, 8),
            Err(Error::InvalidOptions(_))
        ));
        assert!(matches!(
            store.read_batch_owned(&[key(1)], store.tile_bytes - 1, 1),
            Err(Error::BudgetTooSmall { .. })
        ));
        assert_eq!(store.stats().cache_hits, 0);
        assert_eq!(store.stats().cache_misses, 0);
        assert_eq!(store.stats().compressed_bytes_read, 0);
        assert!(store.read_batch_owned(&[], 0, 8).unwrap().is_empty());
        assert_eq!(
            store
                .read_batch_owned(&[key(1)], store.tile_bytes, 8)
                .unwrap(),
            vec![Some(pixels(1, PixelFormat::Rgba8))]
        );
    }

    #[test]
    fn parallel_batch_corruption_returns_no_partial_success_and_joins_other_workers() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = TileStore::create(directory.path(), options(PixelFormat::Rgba8)).unwrap();
        store
            .put_raw(key(0), &pixels(0, PixelFormat::Rgba8))
            .unwrap();
        store
            .put_raw(key(1), &pixels(1, PixelFormat::Rgba8))
            .unwrap();
        store.commit().unwrap();
        let offset = store.index[&key(1)].offset;
        drop(store);
        let mut file = OpenOptions::new()
            .write(true)
            .open(directory.path().join("tiles.store"))
            .unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[254]).unwrap();
        drop(file);
        let mut store = TileStore::open(directory.path(), options(PixelFormat::Rgba8)).unwrap();
        assert!(matches!(
            store.read_batch_owned(&[key(0), key(1)], 2 * store.tile_bytes, 2),
            Err(Error::Corrupt("tile checksum"))
        ));
        assert_eq!(store.stats().cache_misses, 2);
        assert_eq!(store.stats().compressed_bytes_read, store.tile_bytes as u64);
        assert_eq!(store.stats().resident_bytes, 0);
        assert_eq!(
            store.get(key(0)).unwrap().unwrap(),
            pixels(0, PixelFormat::Rgba8)
        );
        store.put(key(2), &pixels(2, PixelFormat::Rgba8)).unwrap();
        store.commit().unwrap();
    }
}
