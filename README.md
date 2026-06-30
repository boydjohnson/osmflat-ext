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

**Phase 1 (Taginfo) is implemented and tested.** The compiler builds the
inverted tag index + histograms (`osmflat-extc --taginfo`), the query side does
key/value binary search + postings + the bbox merge-join, and the fingerprint
guard is wired into `ExtArchive::open`. An end-to-end test
(`osmflat-extc/tests/dc.rs`) builds the sidecar for the district-of-columbia
archive and checks every key, value, and postings list against a brute-force
oracle (2,123 keys / 200k (key,value) pairs). Run it with the sibling
`osmflat-rs` checkout present, or point `OSMFLAT_DC_ARCHIVE` at an archive.

Still stubbed (`todo!()`): the `Backrefs` build (`osmflat-extc/src/build_backrefs.rs`)
and reader (`osmflat-ext/src/backrefs.rs`), non-bbox spatial
(`osmflat-ext/src/spatial.rs`), and taginfo `--combinations`. The Taginfo build
is the in-RAM form; planet-scale mmap scratch is not yet wired up.

The parent is a path dependency on `../osmflat-rs/osmflat` (the
`feature/spatial-index` + `Ids` branch this is designed against).

[osmflat]: https://docs.rs/osmflat
