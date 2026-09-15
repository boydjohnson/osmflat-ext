# osmflat-ext

Sidecar indexes and query support for [osmflat] archives — an **inverted tag
index + taginfo histograms**, **reverse references**, **non-bbox spatial**
queries, and **precomputed multipolygon relation assembly** — built *without
modifying the parent `Osm` archive*.

Everything lives in a separate sibling `Ext` archive whose resources reference
the parent by index. Because the parent stores entities in space-filling-curve
order, the postings in this archive (ascending parent indices) are already
spatially ordered, so a `key=value` postings list merge-joins directly with a
bbox query.

Full design: [`osmflat-ext-design.md`](./osmflat-ext-design.md).

## Layout

```
osmflat-ext/         lib: reader bindings + query API
  flatdata/ext.flatdata   the sidecar schema (regen with flatdata/regen.sh)
  src/ext_generated.rs    generated flatdata bindings (committed)
  src/taginfo.rs          key -> value -> postings queries
  src/backrefs.rs         reverse-reference queries
  src/multipolygon.rs     ring-assembly algorithm (shared) + precomputed-sidecar queries
  src/query.rs            merge-join engine (tag ∩ tag ∩ bbox)
  src/spatial.rs          radius / k-NN / polygon (no sidecar)
  src/fingerprint.rs      parent-staleness guard
osmflat-extc/        bin+lib: the compiler that builds sidecars
  src/build_taginfo.rs    3-phase CSR build (dictionary, count, fill)
  src/build_backrefs.rs   2 CSR builds (node->ways, X->relations)
  src/build_multipolygons.rs  1-pass CSR build (relation ring assembly)
```

## Examples

Runnable examples in `osmflat-ext/examples/` query built sidecars or the parent
archive directly:

```text
osmflat-extc --taginfo --backrefs --out dc.ext district-of-columbia.osmflat
osmflat-extc --combinations --backrefs --out dc-combos.ext district-of-columbia.osmflat
osmflat-extc --multipolygons --out dc.ext district-of-columbia.osmflat
osmflat-extc --coastline --out dc.ext district-of-columbia.osmflat

# taginfo browser (keys table / values table / key=value with example ids)
cargo run --example taginfo  -- district-of-columbia.osmflat dc.ext
cargo run --example taginfo  -- district-of-columbia.osmflat dc.ext highway
cargo run --example taginfo  -- district-of-columbia.osmflat dc-combos.ext highway --combinations
cargo run --example taginfo  -- district-of-columbia.osmflat dc.ext highway crossing
cargo run --example taginfo  -- district-of-columbia.osmflat dc-combos.ext highway crossing --combinations

# reverse references (OSM-id lookup needs the parent built with --reverse-ids)
cargo run --example backrefs -- district-of-columbia.osmflat dc.ext node 281072
cargo run --example backrefs -- district-of-columbia.osmflat dc.ext way 535462113

# non-bbox spatial node queries (no Ext sidecar needed)
cargo run --example spatial -- district-of-columbia.osmflat radius -77.0365 38.8977 0.01
cargo run --example spatial -- district-of-columbia.osmflat nearest -77.0365 38.8977 --k 10
cargo run --example spatial -- district-of-columbia.osmflat polygon -77.04 38.89 -77.01 38.89 -77.01 38.91 -77.04 38.91
```

## Implemented

- `osmflat-extc --taginfo` builds the inverted tag index + histograms; the query
  side does key/value binary search, postings, and the bbox merge-join.
- `osmflat-extc --backrefs` builds node→ways and X→relations reverse indexes;
  the query side slices them in O(deg).
- `ValueView::{nodes,ways,relations}_in_bbox` compose a `key=value ∩ bbox`
  merge-join: bbox candidates come from osmflat's own exact spatial query,
  get run-length compressed to contiguous index ranges, and merge-join the tag
  postings via `query::intersect_bbox` (`O(R·log k)`).
- `spatial::{nodes_within_radius,k_nearest_nodes,nodes_in_polygon}` provide
  node radius, nearest-neighbor, and polygon queries with `f64` lon/lat
  arguments and no extension sidecar.
- `osmflat-extc --combinations` augments Taginfo with per-key co-occurring
  keys (`KeyView::combinations`) and per-tag co-occurring `key=value` pairs
  (`ValueView::combinations`).
- `osmflat-extc --mmap-scratch DIR` backs both sidecar builds' postings and
  offset arrays with unlinked mmap temp files under `DIR` instead of RAM
  (planet scale); the builds are count-then-fill CSR either way, only the
  backing storage changes.
- The fingerprint guard is wired into `ExtArchive::open`.
- `osmflat-extc --multipolygons` assembles every `type=multipolygon`/
  `type=boundary` relation's outer/inner member ways into closed rings
  **once**, at build time, and stores the result as parent node indices (not
  duplicated coordinates, same as every other resource here). This mirrors
  how the wider OSM rendering ecosystem handles the equivalent
  coastline-assembly problem: a dedicated offline tool (`osmcoastline`)
  assembles once and hands renderers already-valid polygons, rather than each
  render query re-stitching rings live. `osmflat_ext::multipolygon` holds the
  assembly algorithm itself (shared by the builder and by any live-fallback
  caller, e.g. `osmflat-mapnik-plugin` when no `--multipolygons` sidecar is
  present) plus `MultipolygonsQuery::polygons(rel_idx)` to read the
  precomputed result with no re-stitching. Relations that aren't area
  relations, or whose outer ways don't close, simply have no entries;
  callers needing the rare leftover *open* chains still need
  `osmflat_ext::multipolygon::assemble_multipolygon` directly.
- `osmflat-extc --coastline` does the equivalent job `osmcoastline` does for
  the standard OSM rendering pipeline: assembles *every* `natural=coastline`
  way in the archive into closed rings, globally (coastline is a plain way
  tag, not relation-based, so this isn't indexed by relation the way
  `Multipolygons` is), classifies each ring's interior as land or water by its
  winding direction (`natural=coastline`'s "land on the left" convention means
  a CCW ring is land, CW is water fully enclosed by coastline), and sorts the
  result by enclosed area descending. That order matters: a renderer that
  draws rings largest-first with a plain painter's algorithm reconstructs
  arbitrarily deep nesting -- an island in a bay in a sea in a larger bay --
  correctly with no explicit hole/exterior pairing at all. `osmflat_ext::coastline`
  holds the assembly + classification (`assemble_coastline`, reusing
  `multipolygon`'s ring-stitcher directly since it doesn't care about
  relations either) and the sidecar reader (`CoastlineQuery::rings()`).
  This is what fixes the open-ocean, Harlem River, and Narrows-connection
  gaps this project found: those are mapped as coastline banks with no
  separate water polygon, so a renderer that only understands `natural=water`
  had nothing to fill; a coastline-derived land/water split fixes all three
  uniformly, the same way it does for the standard OSM pipeline.

  This assembly only ever produces **closed** rings, though (islands, lakes
  fully enclosed by coastline) -- a real mainland coastline is an open chain
  across any bounded extract (there's no tile/bbox frame to close it against
  that isn't arbitrary; a country's real border isn't a rectangle), so
  `--coastline` alone can show NYC's islands as land but never Manhattan's or
  Seattle's *mainland* neighbors. See `--land-polygons` below for that case.
- `osmflat-extc --land-polygons <shapefile>` imports land rings from an
  external, already-closed coastline dataset instead of deriving them from the
  parent archive's own coastline ways -- e.g. osmdata.openstreetmap.de's
  `land-polygons` product (the same one `osm2pgsql`/`openstreetmap-carto`
  production stacks use), whose simplified/split shapefiles are published in
  Web Mercator (EPSG:3857) and get reprojected to WGS84 here. Land/hole is
  taken from the shapefile's own `Outer`/`Inner` ring role (the `shapefile`
  crate canonicalizes every ring's winding to the ESRI convention -- outer
  clockwise, holes counter-clockwise -- regardless of the source data's point
  order), **not** re-derived from signed area the way `Coastline` does: ESRI's
  convention is the opposite sense of `natural=coastline`'s "land on the left"
  rule, so reusing `Coastline`'s area-sign check here would classify every
  landmass as a hole and vice versa. Rings are filtered to the parent
  archive's own bbox and sorted by area descending, same painter's-algorithm
  convention as `Coastline`. `osmflat_ext::land_polygons::LandPolygonsQuery`
  reads the result back; unlike every other resource in this crate it stores
  raw `(lon, lat)` coordinates rather than parent node indices, since this
  data has no other correspondence to the parent archive.

The sidecar compiler has a `test-support` feature that builds synthetic parent
archives through `osmflat/test-support`, then builds extension archives in
memory. These tests validate taginfo, combinations, backrefs, spatial helpers,
bbox merge-join results, multipolygon assembly, and coastline assembly
(precomputed sidecar output checked against live `assemble_multipolygon`/
`assemble_coastline` on the same fixture) against brute-force oracles:

```sh
cargo test -p osmflat-extc --features test-support
```

For the whole workspace:

```sh
cargo test --workspace --all-features
cargo build --workspace --examples --all-features
```

## Current Limitation

The `--combinations` co-occurrence maps are built with in-RAM hash maps even
under `--mmap-scratch`; at planet scale prefer building combinations on a
machine with RAM to match, or skip the flag.

The parent `osmflat` crate is resolved from the `feature/spatial-index` branch
of <https://github.com/boydjohnson/osmflat-rs>.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT License ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.

[osmflat]: https://docs.rs/osmflat
