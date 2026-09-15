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

## Crates

| Crate | Kind | Use it to |
|---|---|---|
| `osmflat-extc` | binary | Build an `Ext` archive from an existing osmflat archive (`osmflat-extc --taginfo --out region.osm.ext region.osm.flat`). |
| `osmflat-ext` | library | Open an `Ext` archive alongside its parent osmflat archive and query it (taginfo, backrefs, multipolygons, coastline/land polygons, bbox merge-joins). |

`osmflat-extc` also exposes a library target — the build logic the binary
wraps, plus the `test-support` fixtures — but applications reading extension
archives should depend on `osmflat-ext`.

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

### Example output

Run against a United States extract (`us.osm.flat`, 50 GB, built with
`osmflatc --reverse-ids`) and its sidecar (`us.osm.ext`, 39 GB, built with
`--taginfo --combinations --backrefs --multipolygons --coastline
--land-polygons`). Each query below returns in about a second or less; output is
trimmed to the first rows.

Top keys:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osm.ext
key                               objects      nodes       ways    values
building                         78395367     377923   77907578      1590
highway                          60301013    9186094   51102904       172
source                           40646848    5056419   35520450     49812
addr:street                      36247345   11961544   24230932    809997
addr:housenumber                 35866011   11967201   23844900    376118
addr:postcode                    28956898   10743012   18171979     78535
addr:city                        28840423   10786895   18010988     25279
addr:state                       26106257    8927173   17141345       284
name                             20038152    2563049   17145795   5483214
service                          12645198        954   12643936       910
...
```

One key's values:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osm.ext highway
highway: 60301013 objects (9186094 nodes, 51102904 ways, 12015 relations), 172 distinct values

value                             objects      nodes       ways      rels
service                          23537944         42   23537703       199
footway                           9518985        159    9516937      1889
residential                       9226790          5    9226780         5
crossing                          4603240    4603188         52         0
track                             2126472         22    2126434        16
turning_circle                    1351990    1351987          3         0
tertiary                          1153342          2    1153338         2
secondary                         1069741          5    1069732         4
...
```

Keys that co-occur with a key:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osm.ext highway --combinations
highway: top co-occurring keys

other key                        together
name                             13430399
service                          12358808
tiger:cfcc                       12130591
tiger:county                     12037572
surface                          11679938
...
```

One `key=value`, with example OSM ids:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osm.ext highway crossing
highway=crossing: 4603240 objects (4603188 nodes, 52 ways, 0 relations)

  example nodes: 12290928756, 12290928764, 13324599559, 13324599561, … (+4603168)
  example ways: 1482653492, 1416589079, 1416589080, 546192330, … (+32)
```

Tags that co-occur with a `key=value`:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osm.ext highway crossing --combinations
highway=crossing: top co-occurring tags

other key                    other value                      together
crossing                     unmarked                          1428944
crossing:markings            no                                1236584
crossing                     uncontrolled                       783482
crossing                     traffic_signals                    542153
crossing:island              no                                 477278
...
```

Reverse references:

```text
$ cargo run --release --example backrefs -- us.osm.flat us.osm.ext node 281072
node 281072 -> index 1354395695

used by 2 way(s):
  way      4681186  highway=service
  way   1383887062  highway=footway

contained in 0 relation(s):

$ cargo run --release --example backrefs -- us.osm.flat us.osm.ext way 535462113
way 535462113 -> index 99694424

contained in 3 relation(s):
  relation     19258305  name=WMATA C11 South Capitol Street Southbound Line
  relation     19258306  name=WMATA C11 South Capitol Street Northbound Line
  relation      9677294  name=South Capitol Street
```

Spatial queries on the parent archive (no sidecar):

```text
$ cargo run --release --example spatial -- us.osm.flat --limit 5 radius -77.0365 38.8977 0.01
37815 node(s) within radius 0.01 of (-77.0365, 38.8977)
  node   8226202329  idx=1356762763 lon=-77.0364883  lat=38.8977038   (no descriptive tags)
  node   4460667769  idx=1355997985 lon=-77.0365317  lat=38.8977038   (no descriptive tags)
  node   4466546707  idx=1355999109 lon=-77.0364643  lat=38.8977023   (no descriptive tags)
  node   8226288017  idx=1356762952 lon=-77.0364639  lat=38.8977020   (no descriptive tags)
  node   4460667768  idx=1355997984 lon=-77.0364638  lat=38.8977038   (no descriptive tags)
  ... (+37810)

$ cargo run --release --example spatial -- us.osm.flat --limit 5 nearest -77.0365 38.8977 --k 10
10 nearest node(s) to (-77.0365, 38.8977)
  node   8226202329  idx=1356762763 lon=-77.0364883  lat=38.8977038   (no descriptive tags)
  node   4460667769  idx=1355997985 lon=-77.0365317  lat=38.8977038   (no descriptive tags)
  node   4466546707  idx=1355999109 lon=-77.0364643  lat=38.8977023   (no descriptive tags)
  node   8226288017  idx=1356762952 lon=-77.0364639  lat=38.8977020   (no descriptive tags)
  node   4460667768  idx=1355997984 lon=-77.0364638  lat=38.8977038   (no descriptive tags)
  ... (+5)

$ cargo run --release --example spatial -- us.osm.flat nearest -125.5 40.0 --k 3
3 nearest node(s) to (-125.5, 40)
  node    527817142  idx=170371021 lon=-124.7155565 lat=40.3799507   (no descriptive tags)
  node    527817140  idx=170371019 lon=-124.7139451 lat=40.3766823   (no descriptive tags)
  node    527817122  idx=170371001 lon=-124.7170933 lat=40.3832401   (no descriptive tags)

$ cargo run --release --example spatial -- us.osm.flat --limit 5 polygon -77.04 38.89 -77.01 38.89 -77.01 38.91 -77.04 38.91
76095 node(s) inside polygon with 4 vertices
  node     49716126  idx=1354417631 lon=-77.0380519  lat=38.8963551   (no descriptive tags)
  node     49718740  idx=1354418053 lon=-77.0318795  lat=38.9059837   (no descriptive tags)
  node     49722737  idx=1354418629 lon=-77.0147297  lat=38.8983426   (no descriptive tags)
  node     49722738  idx=1354418630 lon=-77.0150421  lat=38.8983370   highway=crossing
  node     49722744  idx=1354418633 lon=-77.0151644  lat=38.9064627   (no descriptive tags)
  ... (+76090)
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

`--combinations` is only partly backed by `--mmap-scratch`. The raw tag-pair
mentions are written to scratch in sequential buckets, but the key-pair counts,
each bucket while it is sorted, and the final per-tag co-occurrence lists are
held in RAM. At planet scale prefer building combinations on a machine with RAM
to match, or skip the flag.

The parent `osmflat` crate is resolved from the `main` branch of
<https://github.com/boydjohnson/osmflat-rs>.

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
