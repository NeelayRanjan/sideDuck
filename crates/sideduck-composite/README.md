# Compositor

Pixels are linear-sRGB, premultiplied `[f32; 4]` values. A red pixel with 25%
coverage is `[0.25, 0, 0, 0.25]`. The scalar reference in `blend` applies layer
opacity once, performs blend functions on straight colors, and returns
premultiplied source-over output. `color` handles transfer functions and Display
P3 conversion separately. Add and subtract use clamped color addition and
backdrop-minus-source within the same source-over coverage equation.

## Stack compilation

`cache::compile_plan` receives layers in bottom-to-top order and an active index.
It expands the active layer to its complete contiguous clipping chain, or a
single layer when unclipped, and records that live unit in `active_group`. Visible
layers below that unit become the backdrop dependencies. Above it, maximal runs
of normal, unclipped layers outside backdrop-dependent groups become
`Run::Flattened`. Other individual layers become ordered `Run::Pass` entries, and
each clipping chain becomes one `Run::ClippingGroup` including its base. Invisible
layers do not produce rendering work; an invisible base hides its entire chain.

Premultiplied normal source-over is associative, so a normal run can be composed
over transparent pixels and applied once to the real backdrop. Multiply, screen,
and other backdrop-dependent modes cannot be flattened that way: their colors
depend on the already-composited pixels beneath them. Clipping and non-normal
group context are conservative barriers even when the layer's own mode is normal.
Each clipping chain uses its nearest lower unclipped base. Clipped layers blend
source-atop inside an isolated group, preserving the base's coverage. The base
blend mode and opacity then apply once to the whole group over the real backdrop.
For example, a half-covered base with opaque clipped paint still has alpha 0.5.
Masking the clipped source and then using ordinary source-over would incorrectly
raise that coverage to 0.75. A base cannot be flattened into a preceding run while
its clips remain separate. A leading clip is invalid; an explicit base reference
in the raster layer model must match the nearest lower unclipped base.

The randomized 50-layer test compares compiled execution against the scalar
full-stack reference for different active positions, all blend modes, varying
opacity, visibility, isolated clipping groups, and group barriers. Its absolute tolerance
is `2e-6` per float channel, allowing only reassociation rounding. Multiply and
clipped/group-dependent layers also have explicit tests that they remain barriers.

## Cache dependencies and invalidation

An edit below the active group invalidates the backdrop; an edit above it
invalidates the above-stack cache. An edit anywhere in the live active group
leaves those independent image caches valid. Any clip/group-derived resources must include
their own base/mask revisions in their dependencies. Structural edits invalidate
both partitions.

Changing the active layer changes **both** partitions in general. For example,
moving from index 1 to index 3 adds layers 1 and 2 to the backdrop and removes
layers 2 and 3 from the above stack. The specification's unconditional
“at most one” rebuild claim cannot hold for arbitrary stacks. Exact-key reusable
segment images can avoid recomputing either or both sides; neither side may be
declared unchanged merely because the active index moved in one direction.

`edited_for_plan` accounts for the complete live clipping group. `switched_plans`
compares exact partitions, allowing a selection change within one clipping chain
to retain both caches. The simpler index-only helpers conservatively invalidate
both partitions on any index change.

`CacheKey` stores ordered layer IDs, revisions, blend modes, opacity bits, and
visibility/clip/group flags, plus mip level and tile coordinates. Equality checks
the full dependencies rather than relying on a hash collision assumption.
`ImageCache` uses LRU eviction, counts allocated pixel and dependency capacities,
and reserves conservative per-entry metadata. Its byte budget is independent of
the source tile store and GPU image budget. The process residency gate covers
allocator and driver overhead beyond these ownership counters.

## Viewport and mip tiles

`Viewport::pan` is the screen position of the canvas origin in physical pixels.
The renderer maps the viewport rectangle through the inverse zoom/pan transform,
clips it to the canvas, and intersects only the resulting mip tile range. The
maximum tile bound is exclusive, so an edge exactly on a tile boundary does not
load the next tile. Tests cover negative pan, zoom, off-canvas views, and edges.

`MipPyramid` is sparse and shared across layer IDs. Every reduction averages four
premultiplied linear pixels; missing tiles and pixels outside the canvas contribute
transparent black. Odd canvas edges therefore retain their true fractional area.
The selected level is `floor(log2(1 / zoom))`, clamped to the available pyramid.

The source callback can load a persisted tile at any level. Returning `None` at a
derived level asks the pyramid to generate it from four children; at level zero
it means empty pixels. `tile_with_persistence` sends each generated tile directly
to a sink as a borrowed slice, avoiding an unbounded pending-write collection.
Persisted empty results can be supplied as an all-zero tile if the source must
distinguish a known empty result from an unavailable derived tile.

`invalidate_base_tile` removes only that tile and its ancestor at each level.
`invalidate_region` removes cached tiles intersecting a dirty base-pixel rectangle.
Regeneration is incremental at tile granularity; unrelated tiles and layers remain
cached. The source must also invalidate affected persisted mip tiles when their
base data changes.

The mip budget reserves space for the deepest recursive working set, then uses
the remainder for LRU image and empty-result entries. Source vectors must allocate
at most one full tile. Returned vectors transfer to the caller, which must budget
its retained outputs separately. The reported peak is a conservative cache-plus-
scratch bound. Tests exercise eviction, source errors, sparse tiles, odd edges,
persisted-level loading, and dirty-region regeneration.

`mip_store::generate_mips` builds a persisted RGBA16F level directly from a tile
store through reusable GPU images. It streams source blocks of at most 1024×1024
pixels, reduces them with the same 2×2 linear average, and assembles each output
tile on the GPU before a single readback. Both RGBA8 and RGBA16F sources are
supported. No dense full-resolution layer or complete pyramid is allocated.
At most four block uploads remain pending before a device wait bounds staging
memory. The progress callback runs after each output tile so the full-scale gate
can assert process residency throughout generation. Source and destination tile
caches retain their own explicit budgets; their cost is additional to reported
scratch and GPU image ownership. The destination represents one level, and the
caller owns its generation marker and invalidation after source edits.

GPU mip tests compare every output channel to the scalar pyramid, including
missing source tiles, odd canvas boundaries, both pixel formats, and output tiles
assembled from four bounded source blocks. The absolute tolerance is `0.0005`,
covering the final RGBA16F quantization.

## GPU measurement

GPU work uses persistent images and explicit submissions. Wait for submitted work
before destroying and reallocating a batch, because the driver can retain images
after their Rust owners are dropped. Queue texture writes execute before the next
submission; repeatedly overwriting a source while recording several unsubmitted
passes would make those passes observe the final upload.

A headless device wait establishes completion of GPU work, not presentation.
Presentation gates must use a real surface and call `SurfaceTexture::present`.
An immediate presentation mode, when the adapter advertises it, separates the
render/presentation submission budget from the display's refresh interval.
Neither that API timing nor GPU completion measures physical scanout or
sample-to-photon latency. Report those distinctions with benchmark results.
