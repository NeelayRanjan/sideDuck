//! M1 storage gate: a fixed-size corpus and reproducible adversarial access.
//! Timing measures a fresh process with a warm OS filesystem cache. It does not
//! claim to flush the OS cache or validate compositor and brush budgets.

use std::path::PathBuf;

pub const CANVAS_SIZE: u32 = 8192;
pub const LAYERS: u32 = 500;
pub const TILE_SIZE: u32 = 256;
pub const TILES_PER_SIDE: u32 = CANVAS_SIZE / TILE_SIZE;
pub const TILE_BYTES: usize = (TILE_SIZE * TILE_SIZE * 4) as usize;
pub const STORED_TILES: u32 = LAYERS * TILES_PER_SIDE * TILES_PER_SIDE * 2 / 5;
pub const LOGICAL_BYTES: u64 = STORED_TILES as u64 * TILE_BYTES as u64;
pub const MIB: u64 = 1024 * 1024;
pub const DEFAULT_SEED: u64 = 0x7369_6465_2026_0001;
pub const CORPUS_VERSION: u32 = 1;
pub const EDIT_SLOTS: u32 = 64;
pub const VIEWPORT_TILE_WIDTH: u32 = 10;
pub const VIEWPORT_TILE_HEIGHT: u32 = 7;

#[derive(Clone, Debug)]
pub struct Config {
    pub root: PathBuf,
    pub results: PathBuf,
    pub seconds: u64,
    pub cache_bytes: u64,
    pub ceiling_bytes: u64,
    pub seed: u64,
    pub worker: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root: PathBuf::from("target/storage-gate"),
            results: PathBuf::from("target/gate-results.json"),
            seconds: 2,
            cache_bytes: 64 * MIB,
            ceiling_bytes: 4096 * MIB,
            seed: DEFAULT_SEED,
            worker: false,
        }
    }
}

impl Config {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut config = Self::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            if flag == "--worker" {
                if config.worker {
                    return Err("duplicate --worker flag".into());
                }
                config.worker = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--root" => config.root = PathBuf::from(value),
                "--results" => config.results = PathBuf::from(value),
                "--seconds" => config.seconds = parse_number(&flag, &value)?,
                "--cache-mib" => config.cache_bytes = parse_mib(&flag, &value)?,
                "--ceiling-mib" => config.ceiling_bytes = parse_mib(&flag, &value)?,
                "--seed" => config.seed = parse_number(&flag, &value)?,
                _ => return Err(format!("unknown argument {flag}")),
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=86400).contains(&self.seconds) {
            return Err("--seconds must be between 1 and 86400".into());
        }
        if self.cache_bytes < TILE_BYTES as u64 {
            return Err("cache must hold at least one decoded tile".into());
        }
        if self.ceiling_bytes < 128 * MIB || self.cache_bytes >= self.ceiling_bytes {
            return Err(
                "process ceiling must be at least 128 MiB and exceed the tile cache".into(),
            );
        }
        if self.cache_bytes > usize::MAX as u64 {
            return Err("cache budget exceeds this process address range".into());
        }
        if self.root.as_os_str().is_empty() || self.results.as_os_str().is_empty() {
            return Err("root and result paths must be nonempty".into());
        }
        Ok(())
    }

    pub fn marker(&self) -> String {
        format!(
            "storage-gate-v{CORPUS_VERSION}\nseed={}\ncanvas={CANVAS_SIZE}\nlayers={LAYERS}\ntiles={STORED_TILES}\n",
            self.seed
        )
    }
}

fn parse_number(flag: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} requires an unsigned decimal integer"))
}

fn parse_mib(flag: &str, value: &str) -> Result<u64, String> {
    parse_number(flag, value)?
        .checked_mul(MIB)
        .ok_or_else(|| format!("{flag} overflows a byte count"))
}

/// Exactly two out of every five logical tiles exist, including across layers.
pub fn key_at(ordinal: u32) -> sideduck_tile::TileKey {
    assert!(ordinal < STORED_TILES);
    let slot = ordinal / 2 * 5 + ordinal % 2;
    sideduck_tile::TileKey {
        layer: slot / (TILES_PER_SIDE * TILES_PER_SIDE),
        x: slot % TILES_PER_SIDE,
        y: slot / TILES_PER_SIDE % TILES_PER_SIDE,
    }
}

pub fn edit_key(slot: u32) -> sideduck_tile::TileKey {
    key_at(slot * (STORED_TILES / EDIT_SLOTS))
}

pub fn tile_ordinal(key: sideduck_tile::TileKey) -> u32 {
    let slot = key.layer * TILES_PER_SIDE * TILES_PER_SIDE + key.y * TILES_PER_SIDE + key.x;
    assert!(slot % 5 < 2);
    slot / 5 * 2 + slot % 5
}

/// SplitMix64 is a fully specified integer PRNG, independent of crate versions.
pub struct Random(pub u64);

impl Random {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// Select an occupied tile strictly within the requested visible region.
pub fn visible_key(random: &mut Random, layer: u32, origin: [u32; 2]) -> sideduck_tile::TileKey {
    assert!(layer < LAYERS);
    assert!(origin[0] <= TILES_PER_SIDE - VIEWPORT_TILE_WIDTH);
    assert!(origin[1] <= TILES_PER_SIDE - VIEWPORT_TILE_HEIGHT);
    loop {
        let x = origin[0] + (random.next_u64() % u64::from(VIEWPORT_TILE_WIDTH)) as u32;
        let y = origin[1] + (random.next_u64() % u64::from(VIEWPORT_TILE_HEIGHT)) as u32;
        let slot = layer * TILES_PER_SIDE * TILES_PER_SIDE + y * TILES_PER_SIDE + x;
        if slot % 5 < 2 {
            return sideduck_tile::TileKey { layer, x, y };
        }
    }
}

/// Independently generated, compressible painting-like tile with transparent
/// regions and overlapping colored marks. Every tile has unique geometry and
/// colors derived from its key. This is deliberately not incompressible noise.
pub fn fill_tile(key: sideduck_tile::TileKey, seed: u64, revision: u64, pixels: &mut [u8]) {
    assert_eq!(pixels.len(), TILE_BYTES);
    pixels.fill(0);
    let ordinal = tile_ordinal(key);
    let mut random = Random(seed ^ u64::from(ordinal).wrapping_mul(0xd134_2543_de82_ef95));
    for _ in 0..18 {
        let bits = random.next_u64();
        let left = (bits & 127) as usize;
        let top = ((bits >> 8) & 127) as usize;
        let width = 16 + ((bits >> 16) & 127) as usize;
        let height = 2 + ((bits >> 24) & 31) as usize;
        let color = random.next_u64();
        let alpha = 80 + ((color >> 24) % 176) as u8;
        let rgba = [
            (color as u8).min(alpha),
            ((color >> 8) as u8).min(alpha),
            ((color >> 16) as u8).min(alpha),
            alpha,
        ];
        for row in top..(top + height).min(TILE_SIZE as usize) {
            let start = (row * TILE_SIZE as usize + left) * 4;
            let end = (row * TILE_SIZE as usize + (left + width).min(TILE_SIZE as usize)) * 4;
            for pixel in pixels[start..end].chunks_exact_mut(4) {
                pixel.copy_from_slice(&rgba);
            }
        }
    }
    // Tiny opaque stamps make corpus identity and dirty revisions easy to audit.
    let identity = ordinal.to_le_bytes();
    pixels[0..4].copy_from_slice(&[identity[0], identity[1], identity[2], 255]);
    let revision = revision.to_le_bytes();
    for (index, byte) in revision.into_iter().enumerate() {
        pixels[(index + 1) * 4..(index + 2) * 4].copy_from_slice(&[byte, byte, byte, 255]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn reference_document_8192_500_layers_60_percent_tiles_empty() {
        let keys: BTreeSet<_> = (0..STORED_TILES).map(key_at).collect();
        assert_eq!(keys.len(), 204800);
        assert_eq!(LOGICAL_BYTES, 50 * 1024 * 1024 * 1024);
        assert_eq!(keys.first().unwrap().layer, 0);
        assert_eq!(keys.last().unwrap().layer, 499);
        for key in &keys {
            assert!(key.x < 32 && key.y < 32);
            assert!(tile_ordinal(*key) < STORED_TILES);
        }
        for ordinal in 0..STORED_TILES {
            assert_eq!(tile_ordinal(key_at(ordinal)), ordinal);
        }
    }

    #[test]
    fn corpus_is_deterministic_distinct_and_premultiplied() {
        let mut a = vec![0; TILE_BYTES];
        let mut b = vec![0; TILE_BYTES];
        fill_tile(key_at(230), DEFAULT_SEED, 0, &mut a);
        fill_tile(key_at(230), DEFAULT_SEED, 0, &mut b);
        assert_eq!(a, b);
        fill_tile(key_at(231), DEFAULT_SEED, 0, &mut b);
        assert_ne!(a, b);
        for pixel in a.chunks_exact(4) {
            assert!(pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3]);
        }
        assert!(a.chunks_exact(4).any(|pixel| pixel == [0, 0, 0, 0]));
        fill_tile(key_at(230), DEFAULT_SEED, 1, &mut b);
        assert_ne!(a, b);
    }

    #[test]
    fn budgets_reject_zero_durations_impossible_ceilings_and_overflows() {
        for args in [
            vec!["--seconds", "0"],
            vec!["--seconds", "NaN"],
            vec!["--cache-mib", "0"],
            vec!["--ceiling-mib", "2"],
            vec!["--cache-mib", "4096"],
            vec!["--cache-mib", "18446744073709551615"],
            vec!["--unknown", "1"],
            vec!["--seconds"],
        ] {
            assert!(Config::parse(args.into_iter().map(str::to_owned)).is_err());
        }
        assert!(Config::parse(["--seconds".into(), "600".into()]).is_ok());
    }

    #[test]
    fn dirty_eviction_probe_keys_are_bounded_distinct_and_span_all_layers() {
        let keys: BTreeSet<_> = (0..EDIT_SLOTS).map(edit_key).collect();
        assert_eq!(keys.len(), EDIT_SLOTS as usize);
        assert!(keys.last().unwrap().layer > 490);
    }

    #[test]
    fn random_viewport_jumps_include_bottom_right_edges_without_leaving_viewport() {
        let mut random = Random(DEFAULT_SEED);
        let mut right = false;
        let mut bottom = false;
        for _ in 0..1000 {
            let key = visible_key(&mut random, 499, [22, 25]);
            assert_eq!(key.layer, 499);
            assert!((22..32).contains(&key.x));
            assert!((25..32).contains(&key.y));
            assert!(tile_ordinal(key) < STORED_TILES);
            right |= key.x == 31;
            bottom |= key.y == 31;
        }
        assert!(right && bottom);
    }
}
