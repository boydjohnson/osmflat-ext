# osmflat-ext

Sidecar indexes and query support for [osmflat] archives — an **inverted tag
index + taginfo histograms**, **reverse references**, and **non-bbox spatial**
queries — built *without modifying the parent `Osm` archive*.

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
  src/query.rs            merge-join engine (tag ∩ tag ∩ bbox)
  src/spatial.rs          radius / k-NN / polygon (no sidecar)
  src/fingerprint.rs      parent-staleness guard
osmflat-extc/        bin+lib: the compiler that builds sidecars
  src/build_taginfo.rs    3-phase CSR build (dictionary, count, fill)
  src/build_backrefs.rs   2 CSR builds (node->ways, X->relations)
```

## Examples

Runnable examples in `osmflat-ext/examples/` query built sidecars or the parent
archive directly:

```text
osmflat-extc --taginfo --backrefs --out dc.ext district-of-columbia.osmflat
osmflat-extc --combinations --backrefs --out dc-combos.ext district-of-columbia.osmflat

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
- The fingerprint guard is wired into `ExtArchive::open`.

The sidecar compiler has a `test-support` feature that builds synthetic parent
archives through `osmflat/test-support`, then builds extension archives in
memory. These tests validate taginfo, combinations, backrefs, spatial helpers,
and bbox merge-join results against brute-force oracles:

```sh
cargo test -p osmflat-extc --features test-support
```

For the whole workspace:

```sh
cargo test --workspace --all-features
cargo build --workspace --examples --all-features
```

## Current Limitation

Both sidecar builds currently use the in-RAM builder path. The
`--mmap-scratch` option is reserved for a planet-scale postings build path but
is not wired up yet.

The parent `osmflat` crate is resolved from the `feature/spatial-index` branch
of <https://github.com/boydjohnson/osmflat-rs>.

[osmflat]: https://docs.rs/osmflat
