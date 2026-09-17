# osmflat-ext

Sidecar indexes and query support for [osmflat] archives, built *without
modifying the parent `Osm` archive*:

- **Taginfo**: an inverted tag index with per-type histograms, key and tag
  co-occurrence, values ranked by count, and substring value search.
- **Reverse references**: which ways use a node, which relations contain a
  node, way, or relation.
- **Spatial queries**: radius, k-nearest, and polygon for nodes, ways, and
  relations, on top of osmflat's bbox index (no sidecar needed).
- **Combined queries**: tags, `key=*`, and spatial constraints in one
  `ExtArchive::query()`.
- **Rendering geometry**: precomputed multipolygon rings, coastline rings, and
  imported land polygons.

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
| `osmflat-ext` | library | Open an `Ext` archive alongside its parent osmflat archive and query it (taginfo, backrefs, spatial, combined queries, multipolygons, coastline/land polygons). |

`osmflat-extc` also exposes a library target — the build logic the binary
wraps, plus the `test-support` fixtures — but applications reading extension
archives should depend on `osmflat-ext`.

## Building a sidecar

```text
osmflat-extc [OPTIONS] PARENT
```

| Flag | Builds | Notes |
|---|---|---|
| `--taginfo` | Tag index, histograms, values ranked by count | The base for every other taginfo flag |
| `--combinations` | Key and tag co-occurrence | Implies `--taginfo`; partly held in RAM (see [Limitations](#limitations)) |
| `--key-postings` | Stored `key=*` lists per key | Implies `--taginfo`; readers merge value postings without it |
| `--value-search` | Trigram index for substring value search | Implies `--taginfo`; readers scan without it |
| `--backrefs` | Reverse references | |
| `--multipolygons` | Precomputed multipolygon / boundary rings | |
| `--coastline` | Coastline rings, land/water classified | |
| `--land-polygons PATH` | Land rings imported from a shapefile | e.g. osmdata.openstreetmap.de `land-polygons` |
| `--out DIR` | | Default: `PARENT` with `.ext` appended |
| `--mmap-scratch DIR` | | Back large build arrays with temp files in `DIR` (planet scale) |

At least one sub-archive flag is required. `osmflat-extc --help` lists the
known limitations. OSM-id lookups in the examples need the parent built with
`osmflatc --reverse-ids`.

## Layout

```
osmflat-ext/              lib: reader bindings + query API
  flatdata/ext.flatdata     the sidecar schema (regen with flatdata/regen.sh)
  src/ext_generated.rs      generated flatdata bindings (committed)
  src/lib.rs                ExtArchive: open parent + sidecar, fingerprint check
  src/fingerprint.rs        parent-staleness guard
  src/taginfo.rs            key -> value -> postings, key=*, values by count, substring search
  src/query.rs              merge-join engine and the ExtArchive::query() builder
  src/spatial.rs            radius / k-NN / polygon for nodes, ways, relations (no sidecar)
  src/backrefs.rs           reverse-reference queries
  src/multipolygon.rs       ring assembly (shared with the builder) + precomputed queries
  src/coastline.rs          coastline ring assembly + land/water classification + queries
  src/land_polygons.rs      imported land-polygon queries
  examples/                 taginfo, backrefs, spatial, verify, coastline/land-polygon debugging
  tests/spatial.rs          node spatial queries vs exact scans
osmflat-extc/             bin+lib: the compiler that builds sidecars
  src/main.rs               CLI
  src/build_taginfo.rs      count-then-fill CSR (dictionary, count, fill) + key postings + trigram index
  src/build_backrefs.rs     bucketed parallel sorts (node->ways, X->relations)
  src/build_multipolygons.rs  relation ring assembly
  src/build_coastline.rs    global coastline ring assembly
  src/build_land_polygons.rs  shapefile import, reprojected to WGS84
  src/scratch.rs            RAM- or mmap-backed scratch arrays and bucket sinks (--mmap-scratch)
  src/test_support.rs       synthetic parent + sidecar fixtures (feature test-support)
  tests/                    brute-force oracle tests for every sub-archive and query
```

## Examples

Runnable examples in `osmflat-ext/examples/` query built sidecars or the parent
archive directly:

```text
osmflat-extc --taginfo --backrefs --out dc.ext district-of-columbia.osmflat
osmflat-extc --combinations --value-search --out dc-full.ext district-of-columbia.osmflat

# taginfo browser (keys table / values table / key=value with example ids)
cargo run --example taginfo  -- district-of-columbia.osmflat dc.ext
cargo run --example taginfo  -- district-of-columbia.osmflat dc.ext highway
cargo run --example taginfo  -- district-of-columbia.osmflat dc-full.ext highway --combinations
cargo run --example taginfo  -- district-of-columbia.osmflat dc.ext highway crossing
cargo run --example taginfo  -- district-of-columbia.osmflat dc-full.ext highway crossing --combinations

# substring value search, across all keys or one key (fast with --value-search)
cargo run --example taginfo  -- district-of-columbia.osmflat dc-full.ext --search capitol
cargo run --example taginfo  -- district-of-columbia.osmflat dc-full.ext name --search capitol

# reverse references (OSM-id lookup needs the parent built with --reverse-ids)
cargo run --example backrefs -- district-of-columbia.osmflat dc.ext node 281072
cargo run --example backrefs -- district-of-columbia.osmflat dc.ext way 535462113

# spatial queries (no Ext sidecar needed); --entity way|relation for ways and relations
cargo run --example spatial -- district-of-columbia.osmflat radius -77.0365 38.8977 0.01
cargo run --example spatial -- district-of-columbia.osmflat nearest -77.0365 38.8977 --k 10
cargo run --example spatial -- district-of-columbia.osmflat polygon -77.04 38.89 -77.01 38.89 -77.01 38.91 -77.04 38.91
cargo run --example spatial -- district-of-columbia.osmflat --entity way nearest -77.0365 38.8977 --k 5

# check a built sidecar's layout against its parent
cargo run --release --example verify -- district-of-columbia.osmflat dc-full.ext
```

### Example output

Run against a United States extract (`us.osm.flat`, 50 GB, built with
`osmflatc --reverse-ids`) and its sidecar (`us.osmflat.ext`, 44 GB, built with
`--taginfo --combinations --key-postings --value-search --backrefs
--multipolygons`). Output is trimmed to the first rows.

Top keys:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osmflat.ext
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

One key's values, most common first:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osmflat.ext highway
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
$ cargo run --release --example taginfo -- us.osm.flat us.osmflat.ext highway --combinations
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
$ cargo run --release --example taginfo -- us.osm.flat us.osmflat.ext highway crossing
highway=crossing: 4603240 objects (4603188 nodes, 52 ways, 0 relations)

  example nodes: 12290928756, 12290928764, 13324599559, 13324599561, … (+4603168)
  example ways: 1482653492, 1416589079, 1416589080, 546192330, … (+32)
```

Tags that co-occur with a `key=value`:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osmflat.ext highway crossing --combinations
highway=crossing: top co-occurring tags

other key                    other value                      together
crossing                     unmarked                          1428944
crossing:markings            no                                1236584
crossing                     uncontrolled                       783482
crossing                     traffic_signals                    542153
crossing:island              no                                 477278
...
```

Substring search across all keys, then within one key:

```text
$ cargo run --release --example taginfo -- us.osm.flat us.osmflat.ext --search harriet
645 value(s) containing "harriet" in 33.328625ms (trigram index)

key                      value                                     objects
addr:street              Harriet Avenue                                821
addr:street              Harriet Street                                602
tiger:name_base          Harriet                                       363
addr:street              Harriet Lane                                  280
name                     Harriet Street                                155
...

$ cargo run --release --example taginfo -- us.osm.flat us.osmflat.ext name --search "bde maka"
23 value(s) containing "bde maka" in 6.951625ms (trigram index)

key                      value                                     objects
name                     East Bde Maka Ska Drive                        15
name                     East Bde Maka Ska Parkway                      13
name                     Bde Maka Ska Bike Trail                         5
name                     Bde Maka Ska Bicycle Trail                      3
name                     Bde Maka Ska-Isles                              2
...
```

Reverse references:

```text
$ cargo run --release --example backrefs -- us.osm.flat us.osmflat.ext node 281072
node 281072 -> index 1354395695

used by 2 way(s):
  way      4681186  highway=service
  way   1383887062  highway=footway

contained in 0 relation(s):

$ cargo run --release --example backrefs -- us.osm.flat us.osmflat.ext way 535462113
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

$ cargo run --release --example spatial -- us.osm.flat --entity way nearest -77.0365 38.8977 --k 5
5 nearest way(s) to (-77.0365, 38.8977)
  way         884578463  idx=100109978  name=Center Hall
  way         884552041  idx=100109922  name=Entrance Hall
  way         884552042  idx=100109923  name=Cross Hall
  way         449101149  idx=100109864  name=Center Hall
  way         449101184  idx=100109874  name=Curator

$ cargo run --release --example spatial -- us.osm.flat --entity relation --limit 5 radius -77.0365 38.8977 0.002
31 relation(s) within radius 0.002 of (-77.0365, 38.8977)
  relation      3461864  idx=953984     (no descriptive tags)
  relation     19761182  idx=953986     name=White House
  relation     11158006  idx=953853     name=South Lawn
  relation     20397351  idx=953864     name=White House State Ballroom
  relation     12080412  idx=953980     name=North Lawn
  ... (+26)
```

## Features

### Taginfo (`--taginfo`)

- **Keys and values.** `TaginfoQuery::key`, `keys_with_prefix`, and `kv`;
  per-type counts in O(1) (`KeyView::counts`, `ValueView::counts`).
- **Postings.** `ValueView::{nodes,ways,relations}` are ascending parent
  indices; `ValueView::*_in_bbox` merge-joins them with osmflat's bbox query in
  `O(R·log k)`.
- **Values by count.** `KeyView::values_by_count` lists a key's values most
  common first, with no query-time sort.
- **`key=*`.** `KeyView::{nodes,ways,relations}` and `*_in_bbox` return
  entities carrying a key with any value. With `--key-postings` they read
  stored lists; otherwise they merge the key's value postings. Same result
  either way.
- **Substring search.** `TaginfoQuery::values_containing` and
  `KeyView::values_containing`, ASCII case-insensitive (`"lake"` matches
  `"Lake Harriet"`). With `--value-search` a trigram index narrows the
  candidates; without it, or for patterns under 3 bytes, every value is scanned.
- **Co-occurrence** (`--combinations`). `KeyView::combinations` and
  `ValueView::combinations`, ranked by count.

### Combined queries

`ExtArchive::query()` ANDs tag, `key=*`, and spatial constraints and resolves
them to ascending parent indices:

```rust
let cafes = archive
    .query()
    .with_tag("amenity", "cafe")
    .with_key("wheelchair")
    .within_radius(-77.0365, 38.8977, 0.01)
    .nodes()?; // or .ways() / .relations()
```

Exact tags are intersected smallest-first; `with_key`, `in_bbox`,
`within_radius`, and `in_polygon` clip that set by index ranges; radius and
polygon then run their exact test only on what's left. Invalid input returns a
`QueryError` instead of a wrong result.

### Spatial (`osmflat_ext::spatial`, no sidecar)

`{nodes,ways,relations}_within_radius` (nearest-first),
`k_nearest_{nodes,ways,relations}`, and `{nodes,ways,relations}_in_polygon`.
Ways and relations match when **any part touches**: any segment of a way, or
any member of a relation, recursively. A closed way counts as a line, so it
doesn't match a shape it only encloses. Containment tests are exact integer
math.

### Reverse references (`--backrefs`)

`BackrefsQuery::{ways_using_node, relations_with_node, relations_with_way,
relations_with_relation}`, each an O(degree) slice.

### Rendering geometry

- **`--multipolygons`**: every `type=multipolygon` / `type=boundary`
  relation's outer and inner ways stitched into closed rings once, at build
  time, stored as parent node indices (`MultipolygonsQuery::polygons`). The
  same assembly is available live via `multipolygon::assemble_multipolygon`.
- **`--coastline`**: every `natural=coastline` way stitched into closed rings,
  classified land or water by winding, sorted by area descending so a
  largest-first painter's algorithm nests islands and bays correctly
  (`CoastlineQuery::rings`). Only closed rings result: a mainland coastline in
  a bounded extract never closes.
- **`--land-polygons PATH`**: land rings imported from an already-closed
  dataset (e.g. osmdata.openstreetmap.de `land-polygons`, reprojected from Web
  Mercator), kept whole when they overlap the parent's bbox, for mainland land fill
  (`LandPolygonsQuery`). The one resource that stores coordinates rather than
  parent indices.

### Safety

`ExtArchive::open` checks the sidecar's fingerprint (parent vector lengths,
stringtable length, replication sequence, schema hash) and refuses a sidecar
built for a different parent.

## Testing

The `test-support` feature builds synthetic parent archives and sidecars in
memory. The tests compare every sub-archive and query against brute-force
oracles, including with `--mmap-scratch`:

```sh
cargo test -p osmflat-extc --features test-support
```

For the whole workspace:

```sh
cargo test --workspace --all-features
cargo build --workspace --examples --all-features
```

For a real extract, `cargo run --release --example verify -- PARENT EXT` checks
a built sidecar's layout, sort orders, and counts, and cross-checks a sample of
postings against the parent.

## Limitations

These match `osmflat-extc --help`.

1. **A sidecar is bound to one parent build.** It stores parent indices, so
   opening it against a rebuilt parent fails the fingerprint check. Rebuild the
   sidecar whenever the parent changes.
2. **Build cost scales with the data.** Postings are proportional to the
   parent's tag references; at planet scale use `--mmap-scratch` on a fast
   disk.
3. **`--combinations` is only partly backed by `--mmap-scratch`.** Raw tag
   pairs go to scratch, but key-pair counts, each bucket while it's sorted, and
   the final per-tag lists are held in RAM. At planet scale use a machine with
   RAM to match, or skip the flag.
4. **Substring search is limited.** It needs `--value-search` to be fast,
   folds ASCII case only (non-ASCII bytes must match exactly), and scans for
   patterns under 3 bytes. Keys are exact and prefix searchable only.
5. **No geometry is stored** (except `--land-polygons` coordinates). Spatial
   queries recompute from the parent.
6. **`--coastline` only closes rings**: islands and fully enclosed water. Use
   `--land-polygons` for mainland land fill.
7. **The format changes between `osmflat-extc` versions.** flatdata checks each
   archive's stored schema exactly on open, so a sidecar built by a different
   version fails to open. Rebuild it.

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
