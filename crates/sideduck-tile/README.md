# Tile storage

`TileStore::create` initializes `tiles.store` and `tiles.index` in an existing or
new directory. `open` checks the index, its checksum, blob bounds, and format,
then loads tile data on demand. Canvas dimensions and valid layer identifiers
belong to the document model; tile addresses themselves are unsigned integers.

Pixels are premultiplied linear RGBA8 or little-endian IEEE binary16 RGBA. The
store preserves bytes exactly and does not convert colour spaces. A completely
zero alpha channel (including float negative zero) removes the tile. Nonzero
subnormal alpha is preserved. This assumes valid premultiplied input.

Hot tiles are raw in a byte-budgeted LRU. Dirty eviction compresses with LZ4,
falling back to raw if compression expands data. Reads map one blob temporarily;
no full-store mapping or decoded document buffer exists. Nested pins prevent
eviction, while explicit deletion or an edit clearing the tile invalidates its
pins. Budgets below one tile, or attempts to evict an entirely pinned set, return
errors before exceeding the payload budget. Prefetch is synchronous and accepts
an explicitly bounded list of viewport/neighbourhood addresses.

`put_raw` explicitly keeps a frequently read tile uncompressed when it is flushed.
The raw hint survives loading and editing that tile; replacing it with `put`
restores compression. Both encodings use the same raw-pixel CRC, sparse removal,
admission checks, and commit boundary. Raw storage trades disk space for decode
latency and does not enlarge the decoded cache budget.

Encoding uses pinned `lz4_flex`. The safe decoder follows the official
[LZ4 block format](https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md),
validates every length and offset against the input and output bounds, and copies
overlapping matches by repeatedly doubling an initialized prefix. This avoids
byte-at-a-time expansion of repeated RGBA pixels. Tests compare decoded bytes
with `lz4_flex` for seeded random blocks, long repeated patterns, and mutated
streams; invalid offsets, truncated input, and output overruns return errors.
The independent tile checksum is still checked after every decode.

`read_batch_owned` reads at most 32 addresses with one through eight scoped
workers. It checks the worst-case decoded output size against the caller's
explicit byte allowance before allocating pixels. Returned tiles belong to the
caller and do not enter the LRU; dirty cached pixels take precedence over disk.
Every worker joins before success or failure returns. Additional working memory
is bounded by one mapped blob and an explicit 2 MiB stack per worker, plus the
32-entry task metadata. Callers must include the returned pixels and this scratch
space in their total residency budget.

`commit` is the durability boundary. It writes all dirty blobs, syncs the store,
writes and syncs a checksummed replacement index, renames the index atomically,
and syncs the directory. A process interrupted before publication reopens the
prior committed state; after publication it reopens the new state. Dropping a
store never commits implicitly. Uncommitted trailing blobs remain orphaned;
compaction and operation-log replay belong to later milestones. This provides
process-crash recovery, not a claim of protection against faulty hardware or a
filesystem that does not honor sync/rename guarantees.

The store holds an exclusive file lock. Other writers must honor it, and files
must not be externally modified while open: bounded memory maps rely on existing
blobs remaining immutable. Index validation is eager; blob CRC and decompression
validation are lazy. Opening does not read all pixel data. Corrupt bytes return an
error, rather than silently materializing a blank tile.

`StoreStats.resident_bytes` counts decoded cache payloads. It excludes index and
container allocations, one compressed scratch allocation, and one mapped blob.
Scratch and mapping are each bounded by one tile's worst-case compressed size;
the performance gate separately measures total process resident memory. Dirty
layer invalidations accumulate until `drain_dirty_layers`, independently of
persistence commits. `drain_dirty_tiles` also reports deleted addresses and
evicted edits, deduplicating repeated changes. Drain both after consuming them
each frame; retained invalidation metadata scales with unique changed addresses
since the last drain.

Index v1 is little endian: a 32-byte header (`TILEIDX1`, tile edge u32, format u32,
entry count u64, latest generation u64), sorted 40-byte entries (layer/x/y u32,
generation u64, blob offset u64, stored length u32, raw CRC32 u32, codec u8, three
reserved zero bytes), followed by a CRC32 covering the header and entries.
`tiles.store` starts with `TILEBLB1`, followed by append-only raw/LZ4 blobs. Blob
ranges are checked against file length and tile byte size before mapping.
