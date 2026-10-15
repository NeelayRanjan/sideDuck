# Document packages

`Document::create/open` owns a package directory and an exclusive writer lock.
`apply`, `undo`, and `redo` return only after a complete log record is synced.
Layers use stable numeric IDs, sibling order, explicit group parents, optional
raster masks and clipping bases. Every raster edit carries its structured action
and exact changed tiles. Stroke records retain versioned samples, parameters,
textures and seed through the caller's complete brush JSON snapshot.

The package contains `manifest.json`, `tiles.store`, `tiles.index`, `ops.log`,
`checkpoints/`, `objects/`, `document.lock`, and a valid `thumbnail.png`. Working
pixels are linear-sRGB premultiplied RGBA8 or RGBA16F. Thumbnails are derived
RGBA8 PNGs and do not create undo entries. Original image/PDF assets remain
immutable content-addressed `.source` objects, bounded to 64 MiB per asset.

## Commit and recovery

1. Validate the operation and write changed tile pre/post-images as immutable,
   SHA-256-addressed objects. Lossless LZ4 is used when smaller than raw data.
2. Commit the append-only tile store. Copy its checksummed index to an immutable
   sequence-numbered snapshot, sync the snapshot, then sync its directory.
3. Append one bounded JSON frame containing the operation, state, tile object
   references, index hash, undo history and previous frame hash. The frame ends
   with its SHA-256 and a commit marker; sync the log.

Only step 3 commits the document. Opening scans complete frames, checks ordering,
hashes and state invariants, truncates a torn final frame, and restores the last
committed index snapshot before opening the tile store. Thus an interrupted
tile-store commit cannot expose newer pixels with older layer metadata. A corrupt
middle record is rejected instead of silently discarding subsequent history.

Two recent periodic raster checkpoints are retained, plus the current recovery
snapshot when different. `replay_tiles_from_checkpoint` copies the append-only
blob file with bounded I/O and applies later exact tile post-images one at a time.
Tests compare reconstructed state and every pixel byte, including undo/redo.
This is exact raster replay; re-running brush algorithms from their saved input
is a separate brush integration test, not a claim made by the journal tests.

## Bounds and undo

The decoded tile cache has its own byte ceiling. JSON serialization and parsing
have explicit limits (16 MiB per operation by default, maximum 64 MiB). A large
transaction is rejected and rolled back rather than silently split. Tile images
are processed individually; the caller owns memory retained in its edit iterator.

Undo retains a budgeted set of operation references, charged conservatively for
pre/post-image bytes and structural metadata. Its pixels live on disk in immutable
objects shared with the replay log. Pruning undo does not discard authoritative
operation data: the full operation history and replay objects remain document
content, so total document disk usage can grow. A single operation exceeding the
undo budget commits but does not remain undoable; callers can inspect `can_undo`.
Failed transactions can leave unreachable objects or blob tails; those do not
change committed data and are candidates for the later compaction pass.

Tests cover invalid state, exact replay, undo/reopen, cache bounds, torn log tails,
content corruption, bounded serialization, and a subprocess exiting without
destructors at every commit boundary. The final 500-kill session soak is separate.
