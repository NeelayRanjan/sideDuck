use sideduck_bench::{
    edit_key, fill_tile, key_at, visible_key, Config, Random, CANVAS_SIZE, EDIT_SLOTS, LAYERS,
    LOGICAL_BYTES, MIB, STORED_TILES, TILES_PER_SIDE, TILE_BYTES, TILE_SIZE, VIEWPORT_TILE_HEIGHT,
    VIEWPORT_TILE_WIDTH,
};
use sideduck_platform::process_memory;
use sideduck_tile::{PixelFormat, StoreOptions, TileKey, TileStore};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("Storage budget gate (M1)\n\nUsage: sideduck-bench [--seconds 2] [--root target/storage-gate] [--results target/gate-results.json] [--cache-mib 64] [--ceiling-mib 4096] [--seed DECIMAL]\n\nThe full 8192x8192 / 500-layer corpus always contains 204800 tiles.\nUse --seconds 600 for the ten-minute adversarial residency gate.\nOpen is index-only in a fresh process with the OS cache uncontrolled; this is not a cold-disk benchmark.\nThis gate does not measure compositing, layer-cache rebuilds, or brush throughput.");
        return;
    }
    let result = Config::parse(std::env::args().skip(1))
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message).into())
        .and_then(run);
    if let Err(error) = result {
        eprintln!("Storage budget gate FAILED: {error}");
        std::process::exit(1);
    }
}

fn run(config: Config) -> Result<()> {
    if config.worker {
        return run_worker(&config);
    }
    // An advisory OS lock is released on exit or termination, so a killed gate
    // cannot leave a stale lock. Concurrent gates must use separate roots.
    fs::create_dir_all(&config.root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config.root.join(".gate.lock"))?;
    lock.try_lock().map_err(|error| {
        io::Error::other(format!("cannot exclusively lock the fixture root: {error}"))
    })?;
    let started = Instant::now();
    let mut peak = 0;
    ensure_fixture(&config, &mut peak)?;
    let status = Command::new(std::env::current_exe()?)
        .args(std::env::args().skip(1))
        .arg("--worker")
        .env("STORAGE_GATE_GENERATION_PEAK", peak.to_string())
        .status()?;
    if !status.success() {
        return fail(format!("fresh-process gate exited with {status}"));
    }
    println!(
        "Storage gate passed in {:.2}s including fixture preparation; results: {}",
        started.elapsed().as_secs_f64(),
        config.results.display()
    );
    Ok(())
}

fn options(config: &Config) -> StoreOptions {
    StoreOptions {
        tile_size: TILE_SIZE,
        format: PixelFormat::Rgba8,
        cache_budget_bytes: config.cache_bytes as usize,
    }
}

fn corpus_path(config: &Config) -> PathBuf {
    config.root.join(format!(
        "corpus-v{}-{}",
        sideduck_bench::CORPUS_VERSION,
        config.seed
    ))
}

fn ensure_fixture(config: &Config, peak: &mut u64) -> Result<()> {
    let path = corpus_path(config);
    let expected_marker = config.marker();
    if fs::read_to_string(path.join(".complete")).ok().as_deref() == Some(expected_marker.as_str())
    {
        sample_process(config, peak)?;
        println!("Reusing full storage corpus at {}", path.display());
        return Ok(());
    }
    if path.exists() {
        let owned = fs::read_to_string(path.join(".gate-owned"))?;
        if owned != expected_marker {
            return fail(
                "refusing to replace a fixture directory without this gate's ownership marker",
            );
        }
        fs::remove_dir_all(&path)?;
    }
    fs::create_dir_all(&path)?;
    fs::write(path.join(".gate-owned"), &expected_marker)?;
    let started = Instant::now();
    let mut store = TileStore::create(path.join("store"), options(config))?;
    let mut pixels = vec![0; TILE_BYTES];
    println!(
        "Generating {STORED_TILES} distinct {TILE_SIZE}x{TILE_SIZE} RGBA8 tiles ({:.0} GiB logical); 60% of slots empty",
        LOGICAL_BYTES as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    for ordinal in 0..STORED_TILES {
        let key = key_at(ordinal);
        fill_tile(key, config.seed, 0, &mut pixels);
        store.put(key, &pixels)?;
        if ordinal.is_multiple_of(32) {
            check_budget(&store, config, peak)?;
        }
        if (ordinal + 1).is_multiple_of(16384) {
            println!(
                "Generated {}/{} tiles in {:.1}s (process peak {:.1} MiB)",
                ordinal + 1,
                STORED_TILES,
                started.elapsed().as_secs_f64(),
                *peak as f64 / MIB as f64
            );
        }
    }
    store.commit()?;
    check_budget(&store, config, peak)?;
    if store.tile_count() != STORED_TILES as usize {
        return fail("generated corpus does not contain the required distinct tile count");
    }
    drop(store);
    fs::write(path.join(".complete"), expected_marker)?;
    println!(
        "Fixture ready in {:.2}s; {:.2} MiB on disk. Content uses compressible colored marks, not worst-case noise.",
        started.elapsed().as_secs_f64(),
        directory_bytes(&path)? as f64 / MIB as f64
    );
    Ok(())
}

fn run_worker(config: &Config) -> Result<()> {
    let path = corpus_path(config);
    if fs::read_to_string(path.join(".complete"))? != config.marker() {
        return fail("fixture manifest does not match this gate configuration");
    }
    let mut peak = 0;
    sample_process(config, &mut peak)?;
    let opened = Instant::now();
    let mut store = TileStore::open(path.join("store"), options(config))?;
    let open_seconds = opened.elapsed().as_secs_f64();
    if open_seconds >= 2.0 {
        return fail(format!(
            "open {open_seconds:.6}s exceeds the strict 2s budget"
        ));
    }
    if store.tile_count() != STORED_TILES as usize {
        return fail(format!(
            "expected {STORED_TILES} persisted tiles, found {}",
            store.tile_count()
        ));
    }
    if store.stats().resident_bytes != 0 {
        return fail("index-only open unexpectedly materialized decoded tiles");
    }
    check_budget(&store, config, &mut peak)?;
    let mut pixels = vec![0; TILE_BYTES];
    let mut revisions = BTreeMap::<TileKey, u64>::new();
    // Recover from a previous interrupted gate: only these 64 keys may be edited.
    for slot in 0..EDIT_SLOTS {
        let key = edit_key(slot);
        fill_tile(key, config.seed, 0, &mut pixels);
        store.put(key, &pixels)?;
        revisions.insert(key, 0);
        check_budget(&store, config, &mut peak)?;
    }
    store.commit()?;
    let started = Instant::now();
    let mut progress = Instant::now();
    let mut random = Random(config.seed);
    let mut reads = 0_u64;
    let mut writes = 0_u64;
    let mut viewport_jumps = 0_u64;
    let mut full_comparisons = 0_u64;
    while started.elapsed() < Duration::from_secs(config.seconds) {
        // Jump to a random layer and viewport origin. Four visible occupied
        // tiles are sampled per jump, with no requirement for layer locality.
        let layer = (random.next_u64() % u64::from(LAYERS)) as u32;
        let origin_x =
            (random.next_u64() % u64::from(TILES_PER_SIDE - VIEWPORT_TILE_WIDTH + 1)) as u32;
        let origin_y =
            (random.next_u64() % u64::from(TILES_PER_SIDE - VIEWPORT_TILE_HEIGHT + 1)) as u32;
        for _ in 0..4 {
            let key = visible_key(&mut random, layer, [origin_x, origin_y]);
            let revision = revisions.get(&key).copied().unwrap_or(0);
            let full = reads.is_multiple_of(31);
            verify_tile(&mut store, key, config.seed, revision, &mut pixels, full)?;
            reads += 1;
            full_comparisons += u64::from(full);
            check_budget(&store, config, &mut peak)?;
        }
        if viewport_jumps.is_multiple_of(7) {
            let key = edit_key((random.next_u64() % u64::from(EDIT_SLOTS)) as u32);
            let revision = writes + 1;
            // Exercise the dirty cache API, not just committed direct writes.
            fill_tile(key, config.seed, revision, &mut pixels);
            store.edit(key, |bytes| bytes.copy_from_slice(&pixels))?;
            revisions.insert(key, revision);
            writes += 1;
            check_budget(&store, config, &mut peak)?;
        }
        if viewport_jumps.is_multiple_of(13) {
            let key = edit_key((random.next_u64() % u64::from(EDIT_SLOTS)) as u32);
            verify_tile(
                &mut store,
                key,
                config.seed,
                revisions[&key],
                &mut pixels,
                true,
            )?;
            reads += 1;
            full_comparisons += 1;
            check_budget(&store, config, &mut peak)?;
        }
        viewport_jumps += 1;
        if progress.elapsed() >= Duration::from_secs(30) {
            println!(
                "Residency gate {:.0}/{}s: {reads} reads, {writes} dirty edits, peak {:.1} MiB",
                started.elapsed().as_secs_f64(),
                config.seconds,
                peak as f64 / MIB as f64
            );
            progress = Instant::now();
        }
    }
    let adversarial_seconds = started.elapsed().as_secs_f64();
    let evictions = store.stats().evictions;
    if evictions == 0 {
        return fail(
            "access pattern did not cause eviction; increase duration or reduce the cache",
        );
    }
    store.commit()?;
    check_budget(&store, config, &mut peak)?;
    drop(store);
    // Evictions alone are insufficient: reopen and compare all modified pixels.
    let mut store = TileStore::open(path.join("store"), options(config))?;
    for (&key, &revision) in &revisions {
        verify_tile(&mut store, key, config.seed, revision, &mut pixels, true)?;
        check_budget(&store, config, &mut peak)?;
        full_comparisons += 1;
    }
    for _ in 0..64 {
        let key = key_at((random.next_u64() % u64::from(STORED_TILES)) as u32);
        verify_tile(
            &mut store,
            key,
            config.seed,
            revisions.get(&key).copied().unwrap_or(0),
            &mut pixels,
            true,
        )?;
        check_budget(&store, config, &mut peak)?;
        full_comparisons += 1;
    }
    let generation_peak: u64 = std::env::var("STORAGE_GATE_GENERATION_PEAK")
        .unwrap_or_else(|_| "0".into())
        .parse()?;
    if generation_peak >= config.ceiling_bytes {
        return fail("fixture generation exceeded the process residency ceiling");
    }
    let disk_bytes = directory_bytes(&path)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let parallelism = std::thread::available_parallelism()?.get();
    let result = format!(
        "{{\n  \"gate\": \"M1 tile store\",\n  \"passed\": true,\n  \"unix_timestamp\": {now},\n  \"os\": \"{}\",\n  \"architecture\": \"{}\",\n  \"logical_cpus\": {parallelism},\n  \"seed\": {},\n  \"canvas_size\": {CANVAS_SIZE},\n  \"layers\": {LAYERS},\n  \"tile_size\": {TILE_SIZE},\n  \"persisted_tiles\": {STORED_TILES},\n  \"empty_tile_fraction\": 0.6,\n  \"logical_rgba_bytes\": {LOGICAL_BYTES},\n  \"disk_bytes\": {disk_bytes},\n  \"corpus\": \"distinct compressible colored marks; not worst-case entropy\",\n  \"open_mode\": \"index-only, fresh process, OS cache uncontrolled/warm\",\n  \"open_seconds\": {open_seconds:.9},\n  \"open_budget_seconds\": 2,\n  \"requested_adversarial_seconds\": {},\n  \"adversarial_seconds\": {adversarial_seconds:.6},\n  \"ten_minute_gate_exercised\": {},\n  \"process_peak_bytes\": {peak},\n  \"generation_process_peak_bytes\": {generation_peak},\n  \"process_ceiling_bytes\": {},\n  \"decoded_tile_cache_budget_bytes\": {},\n  \"reads\": {reads},\n  \"dirty_edits\": {writes},\n  \"viewport_jumps\": {viewport_jumps},\n  \"cache_evictions\": {evictions},\n  \"full_pixel_comparisons\": {full_comparisons},\n  \"dirty_reopen_verified_tiles\": {EDIT_SLOTS},\n  \"deferred_budgets\": [\"full canvas composite\", \"active layer cache rebuild\", \"viewport presentation\", \"brush dabs\", \"zoom pyramid\"]\n}}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        config.seed,
        config.seconds,
        config.seconds >= 600,
        config.ceiling_bytes,
        config.cache_bytes,
    );
    if let Some(parent) = config
        .results
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let pending = config.results.with_extension("pending.json");
    fs::write(&pending, result)?;
    fs::rename(&pending, &config.results)?;
    println!(
        "PASS: fresh-process index open {open_seconds:.4}s < 2s; process peak {:.1} MiB < {:.0} MiB; {adversarial_seconds:.2}s adversarial access, {evictions} evictions; dirty pixels verified after reopen",
        peak as f64 / MIB as f64,
        config.ceiling_bytes as f64 / MIB as f64
    );
    Ok(())
}

fn verify_tile(
    store: &mut TileStore,
    key: TileKey,
    seed: u64,
    revision: u64,
    expected: &mut [u8],
    full: bool,
) -> Result<()> {
    let bytes = store
        .get(key)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "persisted tile missing"))?;
    fill_tile(key, seed, revision, expected);
    let length = if full { TILE_BYTES } else { 36 };
    if bytes.len() != TILE_BYTES || bytes[..length] != expected[..length] {
        return fail(format!(
            "tile pixels changed at {key:?}, revision {revision}"
        ));
    }
    Ok(())
}

fn sample_process(config: &Config, peak: &mut u64) -> Result<()> {
    let memory = process_memory()?;
    *peak = (*peak)
        .max(memory.resident_bytes)
        .max(memory.peak_resident_bytes);
    if memory.resident_bytes == 0 || memory.peak_resident_bytes == 0 {
        return fail("platform returned zero resident memory; cannot enforce the process budget");
    }
    if *peak >= config.ceiling_bytes {
        return fail(format!(
            "process resident peak {} bytes exceeds strict ceiling {} bytes",
            *peak, config.ceiling_bytes
        ));
    }
    Ok(())
}

fn check_budget(store: &TileStore, config: &Config, peak: &mut u64) -> Result<()> {
    let stats = store.stats();
    if stats.resident_bytes as u64 > config.cache_bytes
        || stats.peak_resident_bytes as u64 > config.cache_bytes
    {
        return fail("decoded tile cache exceeded its configured byte budget");
    }
    sample_process(config, peak)
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut bytes = 0_u64;
    for item in fs::read_dir(path)? {
        let item = item?;
        let metadata = item.metadata()?;
        bytes = bytes
            .checked_add(if metadata.is_dir() {
                directory_bytes(&item.path())?
            } else {
                metadata.len()
            })
            .ok_or_else(|| io::Error::other("directory byte count overflow"))?;
    }
    Ok(bytes)
}

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(io::Error::other(message.into()).into())
}
