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

Compiling skeleton. The schema, generated bindings, crate split, and full
query/compiler API surface are in place; the bodies are `todo!()` stubs keyed to
the design doc's phases. Build from phase 1 (Taginfo): the dictionary +
count + fill passes in `osmflat-extc/src/build_taginfo.rs`, then the binary
search / merge-join bodies in `osmflat-ext/src/taginfo.rs` and `query.rs`.

The parent is a path dependency on `../osmflat-rs/osmflat` (the
`feature/spatial-index` + `Ids` branch this is designed against).

[osmflat]: https://docs.rs/osmflat
