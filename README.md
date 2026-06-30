# osmflat-ext

Sidecar indexes and query support for [osmflat] archives — an **inverted tag
index + taginfo histograms**, **reverse references**, and **non-bbox spatial**
queries — built *without modifying the parent `Osm` archive*.

Everything lives in a separate sibling `Ext` archive whose resources reference
the parent by index. Because the parent stores entities in space-filling-curve
order, the postings in this archive (ascending parent indices) are already
spatially ordered, so a `key=value` postings list merge-joins directly with a
bbox query.

Full design: [`osmflat-ext-design.md`](../osmflat-ext-design.md).

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

## Status

**Phases 1 (Taginfo) and 2 (Backrefs) are implemented and tested.**

- `osmflat-extc --taginfo` builds the inverted tag index + histograms; the query
  side does key/value binary search, postings, and the bbox merge-join.
- `osmflat-extc --backrefs` builds node→ways and X→relations reverse indexes;
  the query side slices them in O(deg).
- The fingerprint guard is wired into `ExtArchive::open`.

Both are checked end-to-end against the district-of-columbia archive by a
brute-force oracle: `tests/dc.rs` (2,123 keys / 200k (key,value) pairs) and
`tests/dc_backrefs.rs` (1.95M nodes / 283k ways / 5,265 relations / 2.3M
node→way edges). Run with the sibling `osmflat-rs` checkout present, or point
`OSMFLAT_DC_ARCHIVE` at an archive.

Still stubbed (`todo!()`): non-bbox spatial (`osmflat-ext/src/spatial.rs`) and
taginfo `--combinations`. Both sidecar builds are the in-RAM form; planet-scale
mmap scratch is not yet wired up.

The parent is a path dependency on `../osmflat-rs/osmflat` (the
`feature/spatial-index` + `Ids` branch this is designed against).

[osmflat]: https://docs.rs/osmflat
