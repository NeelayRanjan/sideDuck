use super::*;
use std::io::{Seek, SeekFrom};
use tempfile::TempDir;

fn options(tiles: usize) -> StoreOptions {
    StoreOptions {
        tile_size: 8,
        format: PixelFormat::Rgba8,
        cache_budget_bytes: 8 * 8 * 4 * tiles,
    }
}

fn key(layer: u32) -> TileKey {
    TileKey {
        layer,
        x: layer % 4,
        y: layer / 4,
    }
}

fn pixels(value: u8) -> Vec<u8> {
    let mut bytes = vec![value; 8 * 8 * 4];
    for pixel in bytes.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    bytes
}

fn fixture(tiles: usize) -> (TempDir, TileStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = TileStore::create(directory.path(), options(tiles)).unwrap();
    (directory, store)
}

#[test]
fn tile_eviction_preserves_correctness_and_residency_ceiling() {
    let (directory, mut store) = fixture(2);
    for layer in 0..64 {
        store.put(key(layer), &pixels(layer as u8)).unwrap();
        assert!(store.stats().resident_bytes <= options(2).cache_budget_bytes);
    }
    assert_eq!(store.tile_count(), 64);
    assert_eq!(store.keys().collect::<BTreeSet<_>>().len(), 64);
    assert!(store.stats().evictions >= 62);
    store.commit().unwrap();
    drop(store);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.stats().resident_bytes, 0);
    for layer in (0..64).rev() {
        assert_eq!(store.get(key(layer)).unwrap().unwrap(), pixels(layer as u8));
        assert!(store.stats().resident_bytes <= options(1).cache_budget_bytes);
    }
    assert_eq!(store.stats().peak_resident_bytes, 256);
    assert!(store.stats().compressed_bytes_written == 0);
    assert!(store.stats().compressed_bytes_read > 0);
}

#[test]
fn residency_ceiling_rejects_too_small_budget_and_all_pinned() {
    let directory = tempfile::tempdir().unwrap();
    assert!(matches!(
        TileStore::create(directory.path(), options(0)),
        Err(Error::BudgetTooSmall { .. })
    ));
    assert!(!directory.path().join("tiles.store").exists());
    let (_directory, mut store) = fixture(1);
    store.put(key(1), &pixels(1)).unwrap();
    assert!(store.pin(key(1)).unwrap());
    assert!(store.pin(key(1)).unwrap());
    assert!(matches!(
        store.put(key(2), &pixels(2)),
        Err(Error::AllTilesPinned)
    ));
    assert_eq!(store.stats().resident_bytes, 256);
    assert!(store.unpin(key(1)));
    assert!(matches!(
        store.put(key(2), &pixels(2)),
        Err(Error::AllTilesPinned)
    ));
    assert!(store.unpin(key(1)));
    assert!(!store.unpin(key(1)));
    store.put(key(2), &pixels(2)).unwrap();
    assert!(!store.pin(key(999)).unwrap());
    assert!(matches!(
        store.set_cache_budget(1),
        Err(Error::BudgetTooSmall { .. })
    ));
}

#[test]
fn sparse_layers_never_materialize_empty_tiles() {
    let (directory, mut store) = fixture(2);
    assert!(store.get(key(1)).unwrap().is_none());
    assert!(!store.edit(key(1), |_| panic!("empty tile edit")).unwrap());
    let mut zero_alpha = pixels(100);
    for pixel in zero_alpha.chunks_exact_mut(4) {
        pixel[3] = 0;
    }
    store.put(key(1), &zero_alpha).unwrap();
    assert_eq!(store.tile_count(), 0);
    assert_eq!(store.stats().resident_bytes, 0);
    store.put(key(1), &pixels(1)).unwrap();
    store.commit().unwrap();
    store.pin(key(1)).unwrap();
    store.edit(key(1), |bytes| bytes.fill(0)).unwrap();
    assert_eq!(store.tile_count(), 0);
    assert!(!store.unpin(key(1)));
    store.commit().unwrap();
    drop(store);
    let store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.tile_count(), 0);
}

#[test]
fn rgba16f_exact_transparent_removal_preserves_nonzero_alpha() {
    let directory = tempfile::tempdir().unwrap();
    let options = StoreOptions {
        tile_size: 1,
        format: PixelFormat::Rgba16Float,
        cache_budget_bytes: 8,
    };
    let mut store = TileStore::create(directory.path(), options).unwrap();
    store.put(key(1), &[0, 0, 0, 0, 0, 0, 1, 0]).unwrap();
    store.commit().unwrap();
    drop(store);
    let mut store = TileStore::open(directory.path(), options).unwrap();
    assert_eq!(store.get(key(1)).unwrap().unwrap()[6], 1);
    store.put(key(1), &[0, 0, 0, 0, 0, 0, 0, 128]).unwrap();
    assert_eq!(store.tile_count(), 0);
    store.commit().unwrap();
}

#[test]
fn tile_index_round_trips_coordinates_formats_and_generations() {
    let (directory, mut store) = fixture(3);
    let extreme = TileKey {
        layer: u32::MAX,
        x: u32::MAX,
        y: u32::MAX,
    };
    store.put(extreme, &pixels(31)).unwrap();
    store.commit().unwrap();
    store.put(extreme, &pixels(78)).unwrap();
    store.commit().unwrap();
    assert_eq!(store.index[&extreme].generation, 2);
    assert_eq!(store.dirty_layers().collect::<Vec<_>>(), [u32::MAX]);
    assert_eq!(store.drain_dirty_layers().len(), 1);
    assert_eq!(store.dirty_layers().count(), 0);
    drop(store);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.index[&extreme].generation, 2);
    assert_eq!(store.get(extreme).unwrap().unwrap(), pixels(78));
}

#[test]
fn crash_safety_uncommitted_evictions_reopen_last_committed_state() {
    let (directory, mut store) = fixture(1);
    store.put(key(1), &pixels(1)).unwrap();
    store.commit().unwrap();
    let committed_length = store.file.metadata().unwrap().len();
    store.put(key(1), &pixels(2)).unwrap();
    store.put(key(2), &pixels(3)).unwrap(); // Evicts edited tile 1 to an orphan blob.
    store.evict_all().unwrap();
    store.remove(key(1));
    assert!(store.has_uncommitted_changes());
    assert!(store.file.metadata().unwrap().len() > committed_length);
    drop(store); // No implicit commit in Drop.
    fs::write(
        directory.path().join("tiles.index.next"),
        b"interrupted index",
    )
    .unwrap();
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.tile_count(), 1);
    assert_eq!(store.get(key(1)).unwrap().unwrap(), pixels(1));
    assert!(store.get(key(2)).unwrap().is_none());
    store.put(key(3), &pixels(3)).unwrap();
    store.commit().unwrap(); // Recovery overwrites the incomplete next index.
    assert!(!store.has_uncommitted_changes());
}

#[test]
fn dirty_tracking_flushes_edits_before_eviction() {
    let (directory, mut store) = fixture(1);
    store.put(key(1), &pixels(1)).unwrap();
    store.commit().unwrap();
    store.edit(key(1), |bytes| bytes[0] = 79).unwrap();
    store.put(key(2), &pixels(2)).unwrap();
    assert_eq!(store.get(key(1)).unwrap().unwrap()[0], 79);
    assert_eq!(store.dirty_layers().collect::<Vec<_>>(), [1, 2]);
    store.commit().unwrap();
    drop(store);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.get(key(1)).unwrap().unwrap()[0], 79);
}

#[test]
fn lru_prefetch_and_budget_shrink_preserve_pins_and_ceiling() {
    let (_directory, mut store) = fixture(3);
    for layer in 0..4 {
        store.put(key(layer), &pixels(layer as u8)).unwrap();
    }
    store.get(key(1)).unwrap();
    store.put(key(4), &pixels(4)).unwrap();
    assert!(store.cache.contains_key(&key(1)));
    assert!(!store.cache.contains_key(&key(2)));
    store.pin(key(1)).unwrap();
    store.pin(key(4)).unwrap();
    assert!(matches!(
        store.set_cache_budget(256),
        Err(Error::AllTilesPinned)
    ));
    assert_eq!(store.options().cache_budget_bytes, 768);
    assert!(matches!(store.evict_all(), Err(Error::AllTilesPinned)));
    store.unpin(key(1));
    store.unpin(key(4));
    store.set_cache_budget(256).unwrap();
    assert_eq!(store.prefetch([key(0), key(1), key(999)]).unwrap(), 2);
    assert_eq!(store.stats().resident_bytes, 256);
}

#[test]
fn corruption_truncated_store_and_index_are_rejected() {
    for target in ["tiles.store", "tiles.index"] {
        let (directory, mut store) = fixture(1);
        store.put(key(1), &pixels(1)).unwrap();
        store.commit().unwrap();
        drop(store);
        let file = OpenOptions::new()
            .write(true)
            .open(directory.path().join(target))
            .unwrap();
        let length = file.metadata().unwrap().len();
        file.set_len(length - 1).unwrap();
        assert!(matches!(
            TileStore::open(directory.path(), options(1)),
            Err(Error::Corrupt(_))
        ));
    }
}

#[test]
fn corruption_index_checksum_and_tile_checksum_are_rejected() {
    for target in ["tiles.store", "tiles.index"] {
        let (directory, mut store) = fixture(1);
        store.put(key(1), &pixels(1)).unwrap();
        store.commit().unwrap();
        drop(store);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.path().join(target))
            .unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(&[byte[0] ^ 0x20]).unwrap();
        if target == "tiles.index" {
            assert!(matches!(
                TileStore::open(directory.path(), options(1)),
                Err(Error::Corrupt(_))
            ));
        } else {
            let mut reopened = TileStore::open(directory.path(), options(1)).unwrap();
            assert!(matches!(reopened.get(key(1)), Err(Error::Corrupt(_))));
            assert_eq!(reopened.stats().resident_bytes, 0);
        }
    }
}

#[test]
fn exclusive_lock_and_create_never_overwrite_existing_documents() {
    let (directory, store) = fixture(1);
    assert!(matches!(
        TileStore::open(directory.path(), options(1)),
        Err(Error::AlreadyLocked)
    ));
    assert!(TileStore::create(directory.path(), options(1)).is_err());
    drop(store);
    assert!(TileStore::open(directory.path(), options(1)).is_ok());
}

#[test]
fn invalid_options_lengths_and_format_mismatch_return_errors() {
    assert!(StoreOptions {
        tile_size: 0,
        ..options(1)
    }
    .tile_bytes()
    .is_err());
    assert!(StoreOptions {
        tile_size: u32::MAX,
        ..options(1)
    }
    .tile_bytes()
    .is_err());
    assert_eq!(StoreOptions::default().tile_bytes().unwrap(), 256 * 1024);
    let (directory, mut store) = fixture(1);
    assert!(matches!(
        store.put(key(1), &[1, 2, 3]),
        Err(Error::TileLength { .. })
    ));
    assert_eq!(store.tile_count(), 0);
    drop(store);
    let incompatible = StoreOptions {
        tile_size: 4,
        ..options(1)
    };
    assert!(matches!(
        TileStore::open(directory.path(), incompatible),
        Err(Error::FormatMismatch)
    ));
}

#[test]
fn incompressible_raw_tiles_round_trip_without_unbounded_mapping() {
    let (directory, mut store) = fixture(1);
    let mut bytes = pixels(0);
    let mut state = 1_u64;
    for byte in &mut bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    store.put(key(1), &bytes).unwrap();
    store.commit().unwrap();
    assert_eq!(store.index[&key(1)].codec, 0);
    assert_eq!(store.index[&key(1)].length as usize, store.tile_bytes());
    drop(store);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.get(key(1)).unwrap().unwrap(), bytes);
}

#[test]
fn caught_edit_panic_still_marks_dirty_and_drops_transparent_tile_on_save() {
    let (_directory, mut store) = fixture(1);
    store.put(key(1), &pixels(1)).unwrap();
    store.commit().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store
            .edit(key(1), |bytes| {
                bytes.fill(0);
                panic!("interrupted edit");
            })
            .unwrap();
    }));
    assert!(result.is_err());
    assert!(store.has_uncommitted_changes());
    store.commit().unwrap();
    assert_eq!(store.tile_count(), 0);
}

#[test]
fn generation_and_lru_counter_edges_do_not_wrap() {
    let (_directory, mut store) = fixture(2);
    store.put(key(1), &pixels(1)).unwrap();
    store.clock = u64::MAX;
    store.get(key(1)).unwrap();
    assert!(store.clock < u64::MAX);
    store.generation = u64::MAX;
    assert!(matches!(
        store.put(key(2), &pixels(2)),
        Err(Error::InvalidOptions(_))
    ));
    assert_eq!(store.tile_count(), 1);
}

#[test]
fn dirty_tracking_includes_evicted_edited_and_removed_tiles_until_drained() {
    let (_directory, mut store) = fixture(1);
    store.put(key(1), &pixels(1)).unwrap();
    store.edit(key(1), |bytes| bytes[0] = 4).unwrap();
    store.put(key(2), &pixels(2)).unwrap();
    store.remove(key(1));
    store.put(key(2), &[0; 256]).unwrap();
    store.commit().unwrap();
    assert_eq!(store.tile_count(), 0);
    assert_eq!(store.dirty_tiles().collect::<Vec<_>>(), [key(1), key(2)]);
    assert_eq!(store.drain_dirty_tiles().len(), 2);
    assert_eq!(store.dirty_tiles().count(), 0);
    assert_eq!(store.dirty_layers().count(), 2);
}

#[test]
fn corrupt_index_record_bounds_fail_before_any_mmap_or_tile_allocation() {
    let (directory, mut store) = fixture(1);
    store.put(key(1), &pixels(1)).unwrap();
    store.commit().unwrap();
    drop(store);
    let path = directory.path().join("tiles.index");
    let original = fs::read(&path).unwrap();
    let corruptions: Vec<(usize, Vec<u8>)> = vec![
        (16, u64::MAX.to_le_bytes().to_vec()), // Claimed index entry count overflow.
        (24, 0_u64.to_le_bytes().to_vec()),    // Record generation exceeds commit.
        (32 + 20, 0_u64.to_le_bytes().to_vec()), // Blob overlaps store header.
        (32 + 20, u64::MAX.to_le_bytes().to_vec()), // Offset addition overflow.
        (32 + 28, 0_u32.to_le_bytes().to_vec()), // Empty compressed blob.
        (32 + 28, u32::MAX.to_le_bytes().to_vec()), // Blob exceeds tile bound.
        (32 + 36, vec![0]),                    // Raw codec requires exactly the decoded size.
        (32 + 36, vec![2]),                    // Unknown codec.
        (32 + 37, vec![1]),                    // Unknown flags/reserved bytes.
        (12, 9_u32.to_le_bytes().to_vec()),    // Unknown pixel format.
    ];
    for (offset, replacement) in corruptions {
        let mut bytes = original.clone();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        let tail = bytes.len() - 4;
        let checksum = crc32fast::hash(&bytes[..tail]);
        bytes[tail..].copy_from_slice(&checksum.to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(
            matches!(
                TileStore::open(directory.path(), options(1)),
                Err(Error::Corrupt(_))
            ),
            "corruption at {offset}"
        );
    }
}

#[test]
fn corrupt_duplicate_index_keys_are_rejected_even_with_valid_checksum() {
    let (directory, mut store) = fixture(1);
    store.put(key(1), &pixels(1)).unwrap();
    store.put(key(2), &pixels(2)).unwrap();
    store.commit().unwrap();
    drop(store);
    let path = directory.path().join("tiles.index");
    let mut bytes = fs::read(&path).unwrap();
    let duplicate_key = bytes[32..44].to_vec();
    bytes[72..84].copy_from_slice(&duplicate_key);
    let tail = bytes.len() - 4;
    let checksum = crc32fast::hash(&bytes[..tail]);
    bytes[tail..].copy_from_slice(&checksum.to_le_bytes());
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        TileStore::open(directory.path(), options(1)),
        Err(Error::Corrupt(_))
    ));
}

#[test]
fn failed_dirty_eviction_retains_pixels_and_cache_ceiling_for_retry() {
    let (directory, mut store) = fixture(1);
    store.put(key(1), &pixels(19)).unwrap();
    // A read-only file handle injects a deterministic append failure without
    // depending on disk fullness or host permission behavior.
    store.file = File::open(directory.path().join("tiles.store")).unwrap();
    assert!(matches!(store.put(key(2), &pixels(29)), Err(Error::Io(_))));
    assert_eq!(store.stats().resident_bytes, 256);
    assert_eq!(store.get(key(1)).unwrap().unwrap(), pixels(19));
    assert!(store.cache[&key(1)].dirty);
    assert!(!store.contains(key(2)));
    store.file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("tiles.store"))
        .unwrap();
    persistence::lock(&store.file).unwrap();
    store.commit().unwrap();
    drop(store);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.get(key(1)).unwrap().unwrap(), pixels(19));
}

#[test]
fn failed_index_publication_preserves_last_committed_state() {
    let (directory, mut store) = fixture(1);
    store.put(key(1), &pixels(1)).unwrap();
    store.commit().unwrap();
    store.put(key(1), &pixels(2)).unwrap();
    fs::create_dir(directory.path().join("tiles.index.next")).unwrap();
    assert!(store.commit().is_err());
    assert!(store.has_uncommitted_changes());
    drop(store);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.get(key(1)).unwrap().unwrap(), pixels(1));
    fs::remove_dir(directory.path().join("tiles.index.next")).unwrap();
    store.put(key(1), &pixels(3)).unwrap();
    store.commit().unwrap();
    drop(store);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.get(key(1)).unwrap().unwrap(), pixels(3));
}

#[test]
fn property_tile_index_round_trips_generated_edits_removals_and_evictions() {
    let (directory, mut store) = fixture(2);
    let mut reference = BTreeMap::<TileKey, Vec<u8>>::new();
    let mut random = 0x47a1_0b6f_fea8_9b31_u64;
    for step in 0..400 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let address = TileKey {
            layer: (random % 11) as u32,
            x: ((random >> 8) % 5) as u32,
            y: ((random >> 16) % 5) as u32,
        };
        match random % 4 {
            0 | 1 => {
                let bytes = pixels((random >> 24) as u8);
                store.put(address, &bytes).unwrap();
                reference.insert(address, bytes);
            }
            2 => {
                store.put(address, &[0; 256]).unwrap();
                reference.remove(&address);
            }
            _ => {
                let edited = store.edit(address, |bytes| bytes[0] ^= 0x3f).unwrap();
                if let Some(bytes) = reference.get_mut(&address) {
                    bytes[0] ^= 0x3f;
                    assert!(edited);
                } else {
                    assert!(!edited);
                }
            }
        }
        assert_eq!(store.tile_count(), reference.len());
        assert!(store.stats().resident_bytes <= options(2).cache_budget_bytes);
        if step % 53 == 0 {
            store.commit().unwrap();
            drop(store);
            store = TileStore::open(directory.path(), options(2)).unwrap();
            assert_eq!(
                store.keys().collect::<BTreeSet<_>>(),
                reference.keys().copied().collect()
            );
            for (address, expected) in &reference {
                assert_eq!(store.get(*address).unwrap().unwrap(), expected);
            }
        }
    }
}

#[test]
fn raw_hot_tiles_keep_eviction_round_trips_budget_and_sparse_removal() {
    let (directory, mut store) = fixture(1);
    store.put_raw(key(0), &pixels(7)).unwrap();
    store.put(key(1), &pixels(9)).unwrap();
    store.commit().unwrap();
    assert_eq!(store.index[&key(0)].codec, 0);
    assert_eq!(store.index[&key(0)].length as usize, store.tile_bytes());
    assert_eq!(store.index[&key(1)].codec, 1);
    drop(store);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert_eq!(store.get(key(0)).unwrap().unwrap(), pixels(7));
    store.edit(key(0), |bytes| bytes[0] = 4).unwrap();
    store.commit().unwrap();
    assert_eq!(
        store.index[&key(0)].codec,
        0,
        "edits retain the hot storage hint"
    );
    store.put(key(0), &pixels(8)).unwrap();
    store.commit().unwrap();
    assert_eq!(
        store.index[&key(0)].codec,
        1,
        "cold replacement restores compression"
    );
    assert!(store.stats().peak_resident_bytes <= options(1).cache_budget_bytes);
    store.put_raw(key(0), &vec![0; store.tile_bytes()]).unwrap();
    store.commit().unwrap();
    assert!(!store.contains(key(0)));
    assert!(store.put_raw(key(2), &[1]).is_err());
}

#[test]
fn raw_hot_tile_corruption_keeps_checksum_validation() {
    let (directory, mut store) = fixture(1);
    store.put_raw(key(0), &pixels(5)).unwrap();
    store.commit().unwrap();
    drop(store);
    let mut file = OpenOptions::new()
        .write(true)
        .open(directory.path().join("tiles.store"))
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&[0]).unwrap();
    drop(file);
    let mut store = TileStore::open(directory.path(), options(1)).unwrap();
    assert!(matches!(
        store.get(key(0)),
        Err(Error::Corrupt("tile checksum"))
    ));
    assert_eq!(store.stats().resident_bytes, 0);
}
