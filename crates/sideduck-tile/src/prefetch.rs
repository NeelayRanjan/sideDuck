//! One bounded, cancellable prefetch job per store. Workers never mutate the cache.
use crate::{persistence::Record, read_record, Error, Result, TileKey, TileStore};
use std::{
    collections::{BTreeSet, VecDeque},
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

const MAX_KEYS: usize = 32;
const MAX_WORKERS: usize = 8;
const STACK_BYTES: usize = 2 * 1024 * 1024;
const METADATA_BYTES: usize = 64 * 1024;
#[derive(Clone, Copy, Debug)]
pub struct PrefetchOptions {
    pub workers: usize,
    /// Conservative decoded payload + mapped blob payload + worker stacks + metadata bound.
    /// The existing cache is separate, but decoded reservations also consume its ceiling.
    pub max_working_bytes: usize,
}
impl Default for PrefetchOptions {
    fn default() -> Self {
        Self {
            workers: 2,
            max_working_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefetchReport {
    pub ticket: u64,
    pub requested: usize,
    pub scheduled: usize,
    pub loaded: usize,
    pub skipped: usize,
    pub stale: usize,
    pub cancelled: bool,
    pub complete: bool,
    pub reserved_payload_bytes: usize,
    pub working_set_bound_bytes: usize,
}
impl Default for PrefetchReport {
    fn default() -> Self {
        Self {
            ticket: 0,
            requested: 0,
            scheduled: 0,
            loaded: 0,
            skipped: 0,
            stale: 0,
            cancelled: false,
            complete: true,
            reserved_payload_bytes: 0,
            working_set_bound_bytes: 0,
        }
    }
}
struct Decoded {
    key: TileKey,
    record: Record,
    pixels: Result<Vec<u8>>,
}
pub(crate) struct PendingPrefetch {
    cancelled: Arc<AtomicBool>,
    receiver: Receiver<Decoded>,
    workers: Vec<JoinHandle<()>>,
    remaining: usize,
    report: PrefetchReport,
}
impl PendingPrefetch {
    fn join(&mut self) -> Result<()> {
        let mut failed = false;
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                failed = true;
            }
        }
        if failed {
            Err(Error::Io(io::Error::other("prefetch worker panicked")))
        } else {
            Ok(())
        }
    }
}
impl TileStore {
    /// Start at most 32 unique tile reads without blocking on decompression. Existing
    /// cached/dirty or sparse addresses are skipped. Replaces and joins the previous job.
    /// Workers read immutable append-only blobs; only poll_prefetch may admit results.
    /// A record must still match exactly and the address must still be absent from the
    /// cache, so edits/deletions/foreground reads cannot be overwritten by stale work.
    ///
    /// Reservations include every scheduled decoded tile, even before it is read. The
    /// decoded cache plus reservations stays within cache_budget_bytes, and at least
    /// one tile of unreserved capacity remains available to foreground work. The caller
    /// additionally supplies the total job working-set ceiling, including thread stacks.
    pub fn start_prefetch(&mut self, keys: &[TileKey], options: PrefetchOptions) -> Result<u64> {
        if keys.len() > MAX_KEYS || !(1..=MAX_WORKERS).contains(&options.workers) {
            return Err(Error::InvalidOptions(
                "prefetch requires at most 32 keys and 1 through 8 workers",
            ));
        }
        let ticket = self
            .prefetch_sequence
            .checked_add(1)
            .ok_or(Error::InvalidOptions("prefetch ticket overflow"))?;
        let mut seen = BTreeSet::new();
        let mut tasks = VecDeque::new();
        for &key in keys {
            if seen.insert(key) && !self.cache.contains_key(&key) {
                if let Some(record) = self.index.get(&key) {
                    tasks.push_back((key, *record));
                }
            }
        }
        let scheduled = tasks.len();
        let workers = options.workers.min(scheduled);
        let payload = scheduled
            .checked_mul(self.tile_bytes)
            .ok_or(Error::InvalidOptions("prefetch payload overflow"))?;
        let working = if scheduled == 0 {
            0
        } else {
            payload
                .checked_add(
                    workers
                        .checked_mul(self.tile_bytes + STACK_BYTES)
                        .ok_or(Error::InvalidOptions("prefetch working set overflow"))?,
                )
                .and_then(|value| value.checked_add(METADATA_BYTES))
                .ok_or(Error::InvalidOptions("prefetch working set overflow"))?
        };
        if working > options.max_working_bytes {
            return Err(Error::BudgetTooSmall {
                required: working,
                budget: options.max_working_bytes,
            });
        }
        let admission = if scheduled == 0 {
            0
        } else {
            payload
                .checked_add(self.tile_bytes)
                .ok_or(Error::InvalidOptions("prefetch admission overflow"))?
        };
        if admission > self.options.cache_budget_bytes {
            return Err(Error::BudgetTooSmall {
                required: admission,
                budget: self.options.cache_budget_bytes,
            });
        }
        // Cloned file ownership stays within this store's job; Drop joins before the
        // store file/lock is released. Concurrent foreground writes only append.
        let file = Arc::new(self.file.try_clone()?);
        self.cancel_prefetch()?;
        self.make_room(payload)?;
        self.prefetch_sequence = ticket;
        let report = PrefetchReport {
            ticket,
            requested: keys.len(),
            scheduled,
            skipped: keys.len() - scheduled,
            reserved_payload_bytes: payload,
            working_set_bound_bytes: working,
            complete: scheduled == 0,
            ..Default::default()
        };
        self.prefetch_report = report.clone();
        if scheduled == 0 {
            return Ok(ticket);
        }
        self.stats.prefetch_reserved_bytes = payload;
        self.stats.peak_total_resident_bytes = self
            .stats
            .peak_total_resident_bytes
            .max(self.stats.resident_bytes + payload);
        let tasks = Arc::new(Mutex::new(tasks));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel(scheduled);
        let mut job = PendingPrefetch {
            cancelled: cancelled.clone(),
            receiver,
            workers: Vec::with_capacity(workers),
            remaining: scheduled,
            report,
        };
        for _ in 0..workers {
            let tasks = tasks.clone();
            let cancel = cancelled.clone();
            let sender = sender.clone();
            let file = file.clone();
            let tile_bytes = self.tile_bytes;
            match thread::Builder::new()
                .name("tile prefetch".into())
                .stack_size(STACK_BYTES)
                .spawn(move || {
                    while !cancel.load(Ordering::Acquire) {
                        let Some((key, record)) =
                            tasks.lock().expect("prefetch task mutex").pop_front()
                        else {
                            break;
                        };
                        let pixels = read_record(&file, record, tile_bytes);
                        if cancel.load(Ordering::Acquire) {
                            break;
                        }
                        if sender
                            .send(Decoded {
                                key,
                                record,
                                pixels,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }) {
                Ok(worker) => job.workers.push(worker),
                Err(error) => {
                    job.cancelled.store(true, Ordering::Release);
                    let _ = job.join();
                    self.stats.prefetch_reserved_bytes = 0;
                    self.prefetch_report.cancelled = true;
                    self.prefetch_report.complete = true;
                    self.prefetch_report.reserved_payload_bytes = 0;
                    return Err(error.into());
                }
            }
        }
        drop(sender);
        self.prefetch_job = Some(job);
        Ok(ticket)
    }
    /// Admit completed reads without waiting for unfinished work. Results are always clean
    /// cache entries, never logical edits or commits. CRC/decode failures cancel the job.
    pub fn poll_prefetch(&mut self) -> Result<PrefetchReport> {
        let Some(mut job) = self.prefetch_job.take() else {
            return Ok(self.prefetch_report.clone());
        };
        let mut error = None;
        loop {
            let decoded = match job.receiver.try_recv() {
                Ok(value) => value,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if job.remaining != 0 {
                        error = Some(Error::Io(io::Error::other(
                            "prefetch worker ended without a result",
                        )));
                    }
                    break;
                }
            };
            job.remaining -= 1;
            self.stats.prefetch_reserved_bytes -= self.tile_bytes;
            self.stats.cache_misses += 1;
            self.stats.compressed_bytes_read += u64::from(decoded.record.length);
            let pixels = match decoded.pixels {
                Ok(pixels) => pixels,
                Err(cause) => {
                    error = Some(cause);
                    break;
                }
            };
            if self.cache.contains_key(&decoded.key)
                || self.index.get(&decoded.key) != Some(&decoded.record)
            {
                job.report.stale += 1;
                continue;
            }
            if let Err(cause) = self.make_room(self.tile_bytes) {
                error = Some(cause);
                break;
            }
            self.insert_cache(
                decoded.key,
                pixels,
                decoded.record.generation,
                false,
                decoded.record.codec != 0,
            );
            job.report.loaded += 1;
        }
        job.report.reserved_payload_bytes = self.stats.prefetch_reserved_bytes;
        if job.remaining == 0 || error.is_some() {
            if error.is_some() {
                job.cancelled.store(true, Ordering::Release);
                job.report.cancelled = true;
            }
            if let Err(cause) = job.join() {
                if error.is_none() {
                    error = Some(cause);
                }
            }
            self.stats.prefetch_reserved_bytes = 0;
            job.report.reserved_payload_bytes = 0;
            job.report.complete = true;
            self.prefetch_report = job.report;
        } else {
            self.prefetch_report = job.report.clone();
            self.prefetch_job = Some(job);
        }
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(self.prefetch_report.clone())
        }
    }
    /// Join this bounded job, then validate/admit its completed results. Intended for
    /// explicit synchronization and tests; interactive code should poll instead.
    pub fn wait_prefetch(&mut self) -> Result<PrefetchReport> {
        if let Some(job) = &mut self.prefetch_job {
            if let Err(error) = job.join() {
                let _ = self.cancel_prefetch();
                return Err(error);
            }
        }
        self.poll_prefetch()
    }
    /// Cancel outstanding work and join all workers. A worker may finish its current
    /// bounded tile decode; no worker or mapped blob survives this call or store Drop.
    pub fn cancel_prefetch(&mut self) -> Result<PrefetchReport> {
        if let Some(mut job) = self.prefetch_job.take() {
            job.cancelled.store(true, Ordering::Release);
            let result = job.join();
            self.stats.prefetch_reserved_bytes = 0;
            job.report.complete = true;
            job.report.cancelled = true;
            job.report.reserved_payload_bytes = 0;
            self.prefetch_report = job.report;
            result?;
        }
        Ok(self.prefetch_report.clone())
    }
}
impl Drop for TileStore {
    fn drop(&mut self) {
        let _ = self.cancel_prefetch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PixelFormat, StoreOptions};
    use std::{
        fs::OpenOptions,
        io::{Seek, SeekFrom, Write},
    };
    fn key(layer: u32) -> TileKey {
        TileKey { layer, x: 0, y: 0 }
    }
    fn pixels(value: u8) -> Vec<u8> {
        [value, 2, 3, 255].repeat(64)
    }
    fn options(tiles: usize) -> StoreOptions {
        StoreOptions {
            tile_size: 8,
            format: PixelFormat::Rgba8,
            cache_budget_bytes: 256 * tiles,
        }
    }
    fn fixture(tiles: usize, count: u32) -> (tempfile::TempDir, TileStore) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = TileStore::create(directory.path(), options(tiles)).unwrap();
        for index in 0..count {
            store.put(key(index), &pixels(index as u8)).unwrap();
        }
        store.commit().unwrap();
        store.evict_all().unwrap();
        (directory, store)
    }
    #[test]
    fn asynchronous_prefetch_respects_combined_residency_ceiling_and_discards_stale_versions() {
        let (_directory, mut store) = fixture(5, 5);
        let ticket = store
            .start_prefetch(
                &[key(0), key(1), key(2), key(2), key(99)],
                PrefetchOptions::default(),
            )
            .unwrap();
        assert_eq!(store.stats().prefetch_reserved_bytes, 3 * 256);
        store.put(key(0), &pixels(91)).unwrap();
        store.remove(key(1));
        assert_eq!(store.get(key(2)).unwrap().unwrap(), pixels(2));
        assert!(
            store.stats().resident_bytes + store.stats().prefetch_reserved_bytes
                <= options(5).cache_budget_bytes
        );
        let report = store.wait_prefetch().unwrap();
        assert!(report.complete);
        assert_eq!(report.ticket, ticket);
        assert_eq!(report.scheduled, 3);
        assert_eq!(report.skipped, 2);
        assert_eq!(report.stale, 3);
        assert_eq!(report.loaded, 0);
        assert_eq!(report.reserved_payload_bytes, 0);
        assert_eq!(store.get(key(0)).unwrap().unwrap(), pixels(91));
        assert!(store.get(key(1)).unwrap().is_none());
        store
            .start_prefetch(&[key(3), key(4)], PrefetchOptions::default())
            .unwrap();
        let report = store.wait_prefetch().unwrap();
        assert_eq!(report.loaded, 2);
        assert!(store.cache.contains_key(&key(3)));
        assert!(store.cache.contains_key(&key(4)));
        assert!(store.stats().peak_total_resident_bytes <= options(5).cache_budget_bytes);
        assert!(store.has_uncommitted_changes());
        store.commit().unwrap();
    }
    #[test]
    fn asynchronous_prefetch_validates_limits_before_allocating_and_leaves_foreground_capacity() {
        let (_directory, mut store) = fixture(3, 3);
        for config in [
            PrefetchOptions {
                workers: 0,
                ..Default::default()
            },
            PrefetchOptions {
                workers: 9,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                store.start_prefetch(&[key(0)], config),
                Err(Error::InvalidOptions(_))
            ));
        }
        assert!(store
            .start_prefetch(&[key(0); 33], PrefetchOptions::default())
            .is_err());
        assert!(matches!(
            store.start_prefetch(
                &[key(0)],
                PrefetchOptions {
                    workers: 1,
                    max_working_bytes: 1
                }
            ),
            Err(Error::BudgetTooSmall { .. })
        ));
        assert!(matches!(
            store.start_prefetch(&[key(0), key(1), key(2)], PrefetchOptions::default()),
            Err(Error::BudgetTooSmall { .. })
        ));
        assert_eq!(store.stats().prefetch_reserved_bytes, 0);
        assert_eq!(store.stats().resident_bytes, 0);
        store
            .start_prefetch(&[key(0), key(1)], PrefetchOptions::default())
            .unwrap();
        assert!(store.get(key(2)).unwrap().is_some());
        assert_eq!(
            store.stats().resident_bytes + store.stats().prefetch_reserved_bytes,
            options(3).cache_budget_bytes
        );
        store.wait_prefetch().unwrap();
        store
            .start_prefetch(
                &[key(0), key(0), key(99)],
                PrefetchOptions {
                    workers: 1,
                    max_working_bytes: 0,
                },
            )
            .unwrap();
        let report = store.poll_prefetch().unwrap();
        assert_eq!(report.scheduled, 0);
        assert!(report.complete);
        assert_eq!(report.skipped, 3);
        store.prefetch_sequence = u64::MAX;
        assert!(store
            .start_prefetch(&[], PrefetchOptions::default())
            .is_err());
    }
    #[test]
    fn asynchronous_prefetch_cancellation_replacement_budget_shrink_and_drop_join_workers() {
        let (directory, mut store) = fixture(40, 32);
        let keys: Vec<_> = (0..32).map(key).collect();
        let first = store
            .start_prefetch(
                &keys,
                PrefetchOptions {
                    workers: 8,
                    ..Default::default()
                },
            )
            .unwrap();
        let second = store
            .start_prefetch(&[key(31)], PrefetchOptions::default())
            .unwrap();
        assert!(second > first);
        let report = store.cancel_prefetch().unwrap();
        assert!(report.cancelled && report.complete);
        assert_eq!(store.stats().prefetch_reserved_bytes, 0);
        assert_eq!(store.cancel_prefetch().unwrap(), report);
        store
            .start_prefetch(&keys, PrefetchOptions::default())
            .unwrap();
        store.set_cache_budget(256).unwrap();
        assert!(store.poll_prefetch().unwrap().cancelled);
        assert_eq!(store.stats().prefetch_reserved_bytes, 0);
        store
            .set_cache_budget(options(40).cache_budget_bytes)
            .unwrap();
        store
            .start_prefetch(&keys, PrefetchOptions::default())
            .unwrap();
        drop(store);
        let mut reopened = TileStore::open(directory.path(), options(1)).unwrap();
        assert_eq!(reopened.get(key(31)).unwrap().unwrap(), pixels(31));
    }
    #[test]
    fn asynchronous_prefetch_crc_failure_releases_reservations_and_all_workers() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = TileStore::create(directory.path(), options(3)).unwrap();
        store.put_raw(key(0), &pixels(0)).unwrap();
        store.put_raw(key(1), &pixels(1)).unwrap();
        store.commit().unwrap();
        let offset = store.index[&key(0)].offset;
        drop(store);
        let mut file = OpenOptions::new()
            .write(true)
            .open(directory.path().join("tiles.store"))
            .unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[99]).unwrap();
        drop(file);
        let mut store = TileStore::open(directory.path(), options(3)).unwrap();
        store
            .start_prefetch(&[key(0), key(1)], PrefetchOptions::default())
            .unwrap();
        assert!(matches!(
            store.wait_prefetch(),
            Err(Error::Corrupt("tile checksum"))
        ));
        assert_eq!(store.stats().prefetch_reserved_bytes, 0);
        assert!(store.prefetch_job.is_none());
        assert!(!store.cache.contains_key(&key(0)));
        assert_eq!(store.get(key(1)).unwrap().unwrap(), pixels(1));
    }
    #[test]
    fn asynchronous_prefetch_pin_admission_and_nonblocking_poll_preserve_clean_cache() {
        let (_directory, mut store) = fixture(3, 4);
        for index in 0..3 {
            store.pin(key(index)).unwrap();
        }
        assert!(matches!(
            store.start_prefetch(&[key(3)], PrefetchOptions::default()),
            Err(Error::AllTilesPinned)
        ));
        assert_eq!(store.stats().prefetch_reserved_bytes, 0);
        for index in 0..3 {
            store.unpin(key(index));
        }
        store.evict_all().unwrap();
        store
            .start_prefetch(&[key(0)], PrefetchOptions::default())
            .unwrap();
        let early = store.poll_prefetch().unwrap();
        assert!(!early.cancelled);
        let complete = store.wait_prefetch().unwrap();
        assert!(complete.complete);
        assert_eq!(complete.loaded, 1);
        assert!(!store.has_uncommitted_changes());
        assert!(!store.cache[&key(0)].dirty);
    }
}
