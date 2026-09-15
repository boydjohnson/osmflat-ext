# Design: `osmflat-ext` — sidecar indexes & query support for osmflat archives

Status: living design doc. Most of it is implemented; §11 tracks what is done
and what remains. The parent format is the osmflat fork at
<https://github.com/boydjohnson/osmflat-rs> (`main`), where **array order is
the spatial index** (nodes by `ZCurve2D`, ways/relations by `XZ2SFC` of their
bounding box) and the optional `Ids` sub-archive carries OSM ids plus
`*_by_id` permutations for reverse (OSM id → index) lookup.

The authoritative schema is [`osmflat-ext/flatdata/ext.flatdata`](osmflat-ext/flatdata/ext.flatdata);
the schema snippets below show the design shape and may abbreviate it.

This crate adds the capabilities the base format can't do efficiently —
inverted tag index, taginfo-style histograms, reverse references, and non-bbox
spatial — **without modifying the parent `Osm` archive**. Everything is built as
a separate sibling archive that references the parent by index, and every query
stays forward / array-index / binary-search, the grain of the format.

---

## 1. Goal & non-goals

**Goal.** Given an existing osmflat `Osm` archive `A` (produced by `osmflatc`),
produce one or more **sibling sidecar archives** that make the following queries
cheap, and ship a query library that uses them:

1. **Inverted tag index** — `(key,value) → entities`, and `key → values`, by
   type, with O(1) counts. The substrate for a taginfo.openstreetmap.org
   reimplementation.
2. **Taginfo histograms** — per-key and per-(key,value) object counts split by
   node/way/relation; distinct-value counts; key/value search.
3. **Reverse references** — "which ways use node X", "which relations contain
   X", for all member types.
4. **Non-bbox spatial** — k-NN, radius, polygon — built query-side on the
   existing SFC order, no new sidecar.
5. **OSM id → entity** — already provided by `Ids.*_by_id`; the query layer
   just exposes the binary search. Not re-implemented here.

**Non-goals.**
- Not a re-import. The compiler reads the finished `Osm` archive (mmap), never
  the source `.osm.pbf`.
- Does **not** modify the parent archive. Sidecars are separate directories.
- Does **not** heal anything: a sidecar is a pure derived index of `A` as-is.
- Not a substitute for the parent. A sidecar is meaningless without its `A`
  (it stores parent indices, not data).

---

## 2. Packaging: a separate sibling archive, fingerprinted to its parent

A sidecar is an independent flatdata archive in its own directory, e.g.

```
planet.osm.flatdata/          # the parent Osm archive (untouched)
planet.osm.ext/               # the sibling Ext archive
```

The parent `.fbs` is **not** changed (contrast the `ids`/`spatial-index` work,
which were additive *to* the parent). This keeps sidecars build-on-demand,
distribute-separately, delete-freely, and version-independently.

```
namespace osm_ext;

const u64 INVALID_IDX = 0xFFFFFFFFFF;

archive Ext {
    /// Fingerprint of the exact parent archive this sidecar indexes (below).
    header: ExtHeader;
    /// This archive's own strings (the builder version `builder_idx` names).
    stringtable: raw_data;

    /// Inverted tag index + taginfo histograms (§4).
    @optional taginfo: archive Taginfo;

    /// Reverse references (§5).
    @optional backrefs: archive Backrefs;

    /// Precomputed multipolygon relation ring assembly (§5a).
    @optional multipolygons: archive Multipolygons;

    /// Precomputed global coastline ring assembly, land/water classified (§5a).
    @optional coastline: archive Coastline;

    /// Land polygons imported from an external coastline dataset (§5a).
    @optional land_polygons: archive LandPolygons;
}
```

Each capability is an **optional sub-archive**, mirroring `@optional ids:
archive Ids` in the parent: build only what you need (`--taginfo`,
`--backrefs`, `--multipolygons`, `--coastline`, `--land-polygons <shapefile>`).

### The staleness hazard and the fingerprint

A sidecar hard-codes parent **entity indices** and parent **stringtable
indices**. Both change the instant `A` is recompiled or reordered — so a sidecar
is valid for exactly one parent build. This is the ext analog of the
`coord_scale` precondition in the combine design.

`ExtHeader` carries enough to detect a mismatch and refuse to open:

```
struct ExtHeader {
    /// Hash of the parent's embedded schema bytes (`osm.flatdata`).
    parent_schema_hash: u64 : 64;
    /// Parent vector lengths at build time — cheap, catches reorders/rebuilds.
    parent_node_count: u64 : 40;
    parent_way_count: u64 : 40;
    parent_relation_count: u64 : 40;
    parent_tags_count: u64 : 40;
    parent_stringtable_len: u64 : 64;
    /// Parent replication sequence, if present (0 otherwise).
    parent_replication_sequence_number: i64 : 64;
    /// This tool's version (into this archive's own stringtable).
    builder_idx: u64 : 40;
}
```

The query layer opens parent + sidecar together and asserts the fingerprint
before serving any query. Mismatch → hard error ("ext archive built against a
different parent; rebuild").

---

## 3. The lever: the inverted index composes with the spatial index for free

In the spatially ordered parent, **entity index order *is* SFC order**. If tag
postings store parent entity indices **ascending** — which they do naturally
when the compiler fills them by iterating entities in index order — then a
postings list is *already spatially sorted*.

A bbox query returns a small set of contiguous entity-index ranges
`[lo₀,hi₀), [lo₁,hi₁), …` (R of them). Therefore:

```
"highway=primary  ∩  bbox"
  = for each of the R bbox ranges [lo,hi):
        binary-search that subrange inside the tag's postings list
  cost: O(R · log k)        (k = postings length)
```

No temporary hash set, no rescan, no re-sort. This merge-join is the headline
feature and the reason to build the inverted index on a spatially ordered
parent specifically. It generalizes:

- **tag ∩ tag** — merge-join two ascending postings lists, O(k₁+k₂).
- **tag ∩ bbox** — as above.
- **tag ∩ tag ∩ bbox** — chain the merges.

All operands are ascending parent-index runs; everything is a merge.

---

## 4. `Taginfo` sub-archive — key → value → postings

One archive answers items 1 and 2, because taginfo's navigation *is*
key → values → objects.

```
namespace osm_ext;

struct KeyEntry {
    /// Key string, index into the *parent* Osm.stringtable.
    key_idx: u64 : 40;
    /// Entities carrying this key, by type (O(1) key stats).
    count_nodes: u64 : 40;
    count_ways: u64 : 40;
    count_relations: u64 : 40;
    /// Distinct values for this key.
    @range(values) value_first_idx: u64 : 40;
}

struct ValueEntry {
    /// Value string, index into the *parent* Osm.stringtable.
    value_idx: u64 : 40;
    /// Postings: entities having this exact (key,value), by type.
    /// Per-(k,v) per-type counts are these ranges' *lengths* — not stored.
    @range(node_post) node_first_idx: u64 : 40;
    @range(way_post)  way_first_idx:  u64 : 40;
    @range(rel_post)  rel_first_idx:  u64 : 40;
}

/// Index into the parent nodes / ways / relations vector. Ascending within a
/// postings range  ⇒  spatial (SFC) order  ⇒  mergeable with bbox ranges (§3).
struct Ref { value: u64 : 40; }

archive Taginfo {
    /// Distinct keys, **sorted by key string** (not by key_idx). + sentinel.
    keys: vector<KeyEntry>;
    /// Distinct values, grouped by key, **sorted by value string** within each
    /// key's range. + sentinel.
    values: vector<ValueEntry>;
    /// Per-(key,value) postings, one region per type.
    node_post: vector<Ref>;
    way_post:  vector<Ref>;
    rel_post:  vector<Ref>;
    /// builder_idx etc. — handled by Ext.header; no strings needed here.
}
```

### Two economies

1. **No per-(k,v) counts.** They equal the postings range lengths, O(1) to
   derive. Store counts only on `KeyEntry`, since a key's total is not derivable
   without summing its values. (OSM keys are unique per object, so summing value
   counts double-counts nothing.)
2. **Self-contained for tag queries.** Query never touches the parent's
   *unsorted* `tags` / `tags_index`. Text → `keys` (binary search by string) →
   `values` (binary search by string) → postings. The parent stringtable is
   only read to compare strings during those searches.

### What it answers

| query | mechanism | cost |
|---|---|---|
| key prefix search | binary search `keys` (string-sorted) | `O(log K + m)` |
| key stats (counts, #distinct values) | `KeyEntry` + value-range length | `O(1)` |
| values of a key | slice `keys[i]` value range | `O(values)` |
| `(k,v)` count by type | postings range length | `O(1)` |
| entities with `k=v` | slice the three postings | `O(matches)` |
| `k=v ∩ bbox` | merge-join (§3) | `O(R·log k)` |
| `k₁=v₁ ∩ k₂=v₂` | merge-join two postings | `O(k₁+k₂)` |
| key-only `k=*` | union the key's value postings (k-way merge) | `O(Σ matches)` |
| top values for a key | values pre-sortable by count at build, or sort `O(values)` at query | — |

`key=*` (entities with a key regardless of value) is a k-way merge over the
key's value postings; each operand is ascending, so it streams. Implemented as
`KeyView::{nodes, ways, relations}` over `query::union`, with no stored
key-level postings. If `key=*` turns out hot, add an optional fourth postings
region per `KeyEntry` (key-level postings) at the cost of duplicating refs.

### Taginfo "combinations" (`--combinations`)

taginfo's "other keys used by objects that have this key" is a sparse key×key
co-occurrence matrix, and "other tags used by objects that have this tag" a
sparse tag×tag one. Building them means, per entity, enumerating its pairs and
incrementing — heavier than the linear passes above — so they are an optional
addition to `Taginfo`, built only with `--combinations` (which implies
`--taginfo`):

```
struct ComboEntry    { other_key_idx: u64 : 40; together_count: u64 : 40; }
struct TagComboEntry { other_key_idx: u64 : 40; other_value_idx: u64 : 40;
                       together_count: u64 : 40; }
// KeyEntry   += @range(combos)     combo_first_idx:     u64 : 40;
// ValueEntry += @range(tag_combos) tag_combo_first_idx: u64 : 40;
// Taginfo    += combos:     vector<ComboEntry>;     // per key, count desc, then key string
//               tag_combos: vector<TagComboEntry>;  // per tag, count desc, then key/value string
```

Both vectors are empty in a sidecar built without `--combinations`.

**Build.** Tag-pair mentions are radix-bucketed by tag slot into a fixed
number of sequential, append-only streams in one pass (backed by
`--mmap-scratch` at scale), then each bucket is sorted and run-length-encoded
on its own. A direct CSR fill was tried first, but its writes scatter across
the whole mentions array and thrash the page cache on country-scale extracts.
Key-pair counts stay in RAM: they are bounded by the number of distinct key
sets, not by the near-unique per-entity value tags. Each bucket is loaded into
RAM while it is sorted, and the final deduplicated per-tag co-occurrence lists
are held in RAM until they are written out.

---

## 5. `Backrefs` sub-archive — reverse references

Same parallel-range / CSR pattern the parent uses for its own 1:n links. One
range-holder struct per parent entity (+ sentinel), plus a postings vector, for
each reverse relationship:

```
namespace osm_ext;

struct Range { @range(post) first_idx: u64 : 40; }   // generic 1:n range holder
struct Ref   { value: u64 : 40; }                    // parent index, ascending

archive Backrefs {
    /// Parallel to parent `nodes`: ways that reference each node.
    node_ways: vector<Range>;        // + sentinel
    ways_of_node: vector<Ref>;

    /// Relation membership, by member type → parent type.
    node_rels: vector<Range>;  rels_of_node: vector<Ref>;   // parallel to nodes
    way_rels:  vector<Range>;  rels_of_way:  vector<Ref>;   // parallel to ways
    rel_rels:  vector<Range>;  rels_of_rel:  vector<Ref>;   // parallel to relations
}
```

Answers:

| query | mechanism | cost |
|---|---|---|
| ways using node X | slice `node_ways[X]` → `ways_of_node` | `O(deg)` |
| relations containing node/way/relation X | slice the matching `*_rels[X]` | `O(deg)` |
| full geometry of a way's "parents" | backref + parent refs | `O(deg)` |

Built by: one pass over `ways` (each `nodes_index` ref contributes a node→way
backref) and one pass over `relation_members` (each member contributes an
X→relation backref). CSR two-pass (count, then fill); ascending fill order again
gives spatially-ordered postings, so backref results compose with bbox the same
way tags do.

---

## 5a. Rendering sub-archives — multipolygons, coastline, land polygons

Not part of the original taginfo/backrefs scope; added so renderers (e.g.
`osmflat-mapnik-plugin`) get fillable areas without re-assembling rings on
every query. They follow the same sidecar rules: optional, fingerprinted, and
built from the finished parent.

| sub-archive | flag | contents | stores |
|---|---|---|---|
| `Multipolygons` | `--multipolygons` | each `type=multipolygon`/`type=boundary` relation's outer/inner ways stitched into closed rings, once, at build time; relation → polygons → rings → nodes | parent node indices |
| `Coastline` | `--coastline` | every `natural=coastline` way stitched into closed rings, classified land/water by winding ("land on the left"), sorted by area descending | parent node indices |
| `LandPolygons` | `--land-polygons <shapefile>` | rings imported from an external, already-closed dataset (osmdata.openstreetmap.de `land-polygons`, EPSG:3857 reprojected to WGS84), clipped to the parent's bbox, sorted by area descending | raw scaled coordinates |

Sorting rings by area descending lets a renderer paint largest-first and get
arbitrarily deep nesting (island in a bay in a sea) right with no explicit
hole/exterior pairing.

`LandPolygons` is a deliberate exception to "store indices, not data": the
external rings have no correspondence to parent nodes. It exists because
`--coastline` alone can only close islands and enclosed lakes — a mainland
coastline in any bounded extract is an open chain that never closes into a
ring.

Query side: `osmflat_ext::multipolygon` (shared ring-assembly algorithm plus
`MultipolygonsQuery`), `osmflat_ext::coastline` (`CoastlineQuery`), and
`osmflat_ext::land_polygons` (`LandPolygonsQuery`).

---

## 6. The compiler — planet-scale, external, CSR

The compiler (`osmflat-extc`) mmaps `A` and emits the sidecar. Because the
target is **planet** scale, postings are built with an **external** count-then-
fill rather than assuming everything fits in RAM. The pattern matches
`osmflatc`'s `--scratch-dir`-on-SSD philosophy.

### 6.1 Sizing the problem

Postings total ≈ `tags_index.len()` entries (one per (entity,tag) occurrence)
for the tag index, and ≈ `nodes_index.len()` + `Σ members` for backrefs. Each
`Ref` is 5 bytes (u40). Planet `tags_index` is on the order of low-billions →
single-digit GB of postings. The dictionaries (`keys`, `values`) are small:
~10⁵ distinct keys, ~10⁸ distinct values worst case but typically far fewer.

### 6.2 Tag index build (three phases)

```
Phase 0  Dictionary.
  Scan tags_index → tags → (key_idx, value_idx) once, collecting the set of
  distinct (key,value) and grouping value_idx under key_idx. Sort keys by
  *string* (resolve key_idx in parent stringtable); within each key, sort
  values by string. This fixes the slot order of every (k,v) and assigns each
  a dense tag-slot id `t`. Output the `keys`/`values` vectors (counts filled in
  Phase 2). Memory: distinct (k,v) only — fits in RAM even at planet
  (a few hundred MB), but can spill to a sorted run if needed.

Phase 1  Count.
  For each type, iterate entities in index order; for each tag occurrence map
  (key,value) → slot t (hash lookup) → bump per-(t,type) counter. Prefix-sum
  the counters into postings offsets (CSR row pointers = the @range first_idx
  fields). This sets ValueEntry.{node,way,rel}_first_idx and the sentinel.

Phase 2  Fill.
  Iterate entities in index order again; append entity index into
  post[type][ offset[t,type]++ ]. In-order iteration ⇒ each postings run comes
  out ascending = spatial order (§3), no sort. Simultaneously accumulate the
  KeyEntry per-type counts.
```

Phases 1–2 are two linear passes over the parent; the postings array is written
through an mmap-backed temp (or directly into the flatdata vector if RAM
allows). The slot map `(key,value) → t` is the one structure that must stay
resident; key it on `(key_idx,value_idx)` packed into u80→u128 or a 64-bit hash
with collision check against the parent.

**Alternative, fully external (no resident slot map):** emit
`(t, type, entity_idx)` tuples to scratch, external-sort by `(t,type,entity)`,
then stream-write postings. Trades the resident map for disk I/O; pick per the
`--mmap`/RAM budget, same dial as the combine design's perm arrays.

### 6.3 Backrefs build

Two CSR two-pass builds (node→ways from `nodes_index`; X→relations from
`relation_members`), structurally identical to 6.2 Phases 1–2 but keyed on the
parent index rather than a tag slot — so no dictionary phase.

### 6.4 Memory / scratch budget

| structure | size (planet, order) | lifetime | notes |
|---|---|---|---|
| `(key,value) → slot` map | ~distinct (k,v) × ~16 B | Phases 0–2 | resident, or replaced by external sort |
| tag postings | ~`tags_index.len()` × 5 B | written out | mmap temp at planet |
| CSR offset counters | ~slots × 3 × 8 B | Phases 1–2 | resident, small |
| backref postings | ~(`nodes_index`+`Σmembers`) × 5 B | written out | mmap temp |
| dictionaries (`keys`,`values`) | small | output | — |

---

## 7. Query API (`osmflat-ext` lib)

Opens parent + `Ext` together, checks the fingerprint, then:

```rust
let osm = osmflat::Osm::open(parent)?;
let ext = osmflat_ext::Ext::open(sidecar, &osm)?;   // verifies §2 fingerprint

// taginfo
ext.tags().key("highway")?.stats();                 // O(1) counts by type
ext.tags().key("highway")?.values();                // iterator of (value, counts)
ext.tags().kv("highway", "primary")?.nodes();       // ascending node indices
ext.tags().kv("highway", "primary")?.ways_in_bbox(bbox);   // merge-join (§3)
ext.tags().search_keys_prefix("addr:");             // binary-search scan

// boolean composition — all merge-joins over ascending runs
ext.query()
   .with_tag("highway", "primary")
   .with_tag("oneway", "yes")
   .in_bbox(bbox)
   .ways();

// backrefs
ext.backrefs().ways_using_node(node_idx);
ext.backrefs().relations_containing_way(way_idx);

// id lookup — over the *parent* Ids permutation, not a new sidecar
osm.node_by_osm_id(123)?;                            // binary search ids.nodes_by_id
```

**As implemented**, the names differ from the sketch above:

| sketch | implemented |
|---|---|
| `Ext::open(sidecar, &osm)` | `ExtArchive::open(parent, ext)` (fingerprint checked, returns `fingerprint::Mismatch`) |
| `ext.tags()` | `ExtArchive::taginfo() -> Option<TaginfoQuery>` |
| `.key(k)?.stats()` | `KeyView::counts()`, `KeyView::distinct_values()` |
| `search_keys_prefix` | `TaginfoQuery::keys_with_prefix` |
| `kv(k, v)?.nodes()` / `ways_in_bbox` | `ValueView::{nodes, ways, relations}` and `{nodes, ways, relations}_in_bbox` |
| `k=*` | `KeyView::{nodes, ways, relations}` and `{nodes, ways, relations}_in_bbox` |
| `ext.query().with_tag(..).in_bbox(..).ways()` | `ExtArchive::query() -> query::Query`: `with_tag`, `with_key`, `in_bbox`, `within_radius`, `in_polygon` (all ANDed), resolved by `nodes()` / `ways()` / `relations()` to `Result<Vec<u64>, QueryError>`. Lower level: `query::Selection` and `query::{intersect, union, intersect_bbox}` |
| `backrefs().relations_containing_way` | `BackrefsQuery::{ways_using_node, relations_with_node, relations_with_way, relations_with_relation}` |
| `osm.node_by_osm_id` | `osmflat::{node_idx_by_id, way_idx_by_id, relation_idx_by_id}` in the parent crate |

### Query-side non-bbox spatial (item 4) — no sidecar

Built on the existing SFC order; pure lib code in `osmflat_ext::spatial`, for
nodes, ways, and relations:

- **radius** (`{nodes,ways,relations}_within_radius`) — one bbox query around
  the point, refined by an exact distance test, nearest-first.
- **k-NN** (`k_nearest_{nodes,ways,relations}`) — expanding square: query a
  small square around the point, keep the k best in a bounded heap, and double
  the square until the k-th best distance is within its half-width (every
  closer entity then has a point inside the square, so its bbox overlaps it and
  the result is exact). `O(k)` memory.
- **polygon** (`{nodes,ways,relations}_in_polygon`) — bbox-of-polygon prefilter
  via the spatial query, then an exact test.

**Geometry semantics: any part touches.** A node is its point. A way is its
resolved node sequence as a linestring (unresolvable refs dropped; a
single-node way is a point); it matches a radius if any segment comes within
it, and a polygon if any vertex is inside or any segment crosses an edge
(boundary included). A closed way is still a line: it doesn't match a shape it
merely encloses. A relation matches if any member node, member way, or member
relation (recursively, each relation once, so cycles terminate) does. Its
k-NN / radius distance is to its nearest member geometry.

All containment tests are exact integer math in archive units: point-segment
distance compares `cross² ≤ r²·len²` in `u128` (a float fallback only for
globe-spanning overflow), and segment intersection uses orientation signs with
collinear-overlap handling. k-NN ordering for ways and relations uses `f64`
distances, so it settles one archive unit early to stay exact.

Candidates come from osmflat's bbox queries — a way's recomputed bbox, a
relation's **stored** bbox — so a relation whose stored bbox doesn't cover its
members can be missed. Query boxes are clamped to the world first; osmflat's
way / relation curves panic on a box past ±180 / ±90.

**Composing with tag filters** goes through `ExtArchive::query()`
(`query::Query`). Exact tags are intersected smallest-first; `with_key`
(`key=*`) and spatial constraints then clip that set as ascending entity-index
ranges (a range merge-join, §3), and a radius or polygon constraint runs its
exact test only on the survivors. With no exact tags, spatial constraints run
before `key=*` so a large key's union is clipped early. `key=*` clipping
leapfrogs each value's postings against the ranges (binary-searching past
gaps), so it costs about the smaller of the postings and the ranges. The exact
tests are the same `RadiusFilter` / `PolygonFilter` the standalone functions
use, so both agree on every edge case. A tag or key that doesn't exist
short-circuits to an empty result before any spatial work.

---

## 8. Crate / binary structure

Mirror the `osmflat` (lib) / `osmflatc` (lib+bin) split:

```
osmflat-ext        (lib)      flatdata reader bindings for the Ext archive
                              + query API (§7) + non-bbox spatial
osmflat-extc       (lib+bin)  the compiler (§6); the lib holds the build code
                              the binary wraps, plus `test-support` fixtures
```

Id lookup lives in the parent `osmflat` crate (`osmflat::ids`), not here.

`osmflat-extc` depends on `osmflat` for the reader and reuses nothing of the
parent's writer except the flatdata builder for the sidecar. CLI:

```
osmflat-extc [OPTIONS] PARENT
  PARENT                    input osmflat archive dir
  --out DIR                 output Ext archive dir (default: PARENT sibling .ext)
  --taginfo                 build the Taginfo sub-archive
  --combinations            also build taginfo key/tag co-occurrence (implies --taginfo)
  --backrefs                build the Backrefs sub-archive
  --multipolygons           build the Multipolygons sub-archive (§5a)
  --coastline               build the Coastline sub-archive (§5a)
  --land-polygons PATH      build the LandPolygons sub-archive from a shapefile (§5a)
  --mmap-scratch DIR        back the build with mmap temp files here (planet scale)
```

At least one sub-archive flag is required. `--help` also lists the §10
limitations.

---

## 9. Testing strategy

- **Unit (in-memory).** Reuse `osmflat::test_support::build_archive` to build a
  small parent with known tags/refs/members; run the compiler; assert:
  - sidecar opens and its fingerprint matches the parent;
  - `keys`/`values` are string-sorted; every vector ends with a valid sentinel;
  - per-(k,v) postings range lengths equal a brute-force scan of the parent;
  - postings are strictly ascending (the spatial-order invariant §3);
  - backref `ways_using_node` matches a brute-force scan.
- **Differential vs. brute force.** For a real extract, compare every ext query
  against an O(n)-scan oracle over the parent: key counts, (k,v) membership,
  backrefs. The brute-force scan is the "what it can't do efficiently" baseline
  the index replaces, so it's the natural oracle.
- **Spatial composition.** `k=v ∩ bbox` via merge-join must equal
  `{e ∈ find_*_by_bounding_box(bbox) : has_tag(e, k, v)}`.
- **Staleness.** Mutate the parent (rebuild), confirm the sidecar refuses to
  open against it.
- **Property.** Postings of `k=v` ⊆ entities of `k=*`; Σ value counts of a key =
  key count; merge-join is commutative in the result set.

**Where these live.**

| item | status |
|---|---|
| unit oracles: taginfo, combinations, backrefs, `k=v ∩ bbox` (each also with `--mmap-scratch`) | `osmflat-extc/tests/synthetic.rs` |
| multipolygon / coastline precomputed-vs-live, land-polygon import | `osmflat-extc/tests/{multipolygons,coastline,land_polygons}.rs` |
| non-bbox spatial vs. exact scan (k-NN across dense ties, sparse, world-corner centers) | `osmflat-ext/tests/spatial.rs` |
| staleness: identical rebuild opens; extra node/way/relation/tag or longer string refuses | `osmflat-extc/tests/staleness.rs` (schema-hash and replication-sequence mismatches are not exercised: the fixture can't vary them) |
| `k=v ⊆ k=*` and `k=*` length = key count | `osmflat-extc/tests/synthetic.rs` |
| `Query` = intersection of each constraint computed on its own (tag / `key=*` scan, bbox query, standalone radius/polygon), across tag × key × spatial combinations and all three entity types; `KeyView::*_in_bbox`; boxes past the world edge; error cases | `osmflat-extc/tests/query_builder.rs` |
| way / relation radius, polygon, k-NN vs. a brute-force oracle reading the parent with no prefilter (single-node ways, nested / self / cyclic relations, concave and sliver polygons, far and world-corner centers) | `osmflat-extc/tests/way_spatial.rs`; exact primitives at the boundary in `osmflat-ext/src/spatial.rs` unit tests |
| differential on a real extract | `cargo run --release --example verify` (layout, bounds, sort orders, count sums; sampled membership cross-check) |
| merge-join commutativity | not yet |

---

## 10. Known limitations (state in `--help` / README)

1. **Sidecar is parent-bound.** Stores parent indices; invalid against any
   other/rebuilt parent (caught by the §2 fingerprint, not silently).
2. **Build cost ≈ data size.** Postings are O(tags_index); planet needs scratch
   disk, like osmflatc's RocksDB phase.
3. **Substring value search not indexed.** `keys`/`values` are prefix-searchable
   (string-sorted); arbitrary substring search over ~10⁸ planet values is a scan
   or a future n-gram sidecar.
4. **Combinations are only partly external.** `--combinations` spills raw
   tag-pair mentions to `--mmap-scratch`, but key-pair counts, each bucket while
   it is sorted, and the final per-tag co-occurrence lists stay resident; at
   planet scale build combinations on a machine with RAM to match, or skip the
   flag.
5. **No geometry stored.** Spatial refinement (radius/polygon) recomputes from
   the parent, inheriting the way-bbox recompute cost noted in the spatial
   summary. (`LandPolygons` is the one exception: it stores imported
   coordinates.)
6. **`--coastline` only closes rings.** A mainland coastline in a bounded
   extract is an open chain, so only islands and enclosed water become areas;
   use `--land-polygons` for mainland land fill.

---

## 11. Phased roadmap

1. **Done.** `Taginfo` schema + compiler (`--taginfo`, phases 0–2) + query API
   + `k=v ∩ bbox` merge-join. **(the taginfo.openstreetmap core)**
2. **Done.** Query lib polish: non-bbox spatial (radius, k-NN, polygon) for
   nodes, ways, and relations; `key=*` via `KeyView`, within a bbox, and as a
   `Query` constraint; id lookup (provided by the parent `osmflat::ids`); query
   builder (`ExtArchive::query()`) composing tags and keys with bbox / radius /
   polygon.
3. **Done.** `Backrefs` (`--backrefs`).
4. **Partly done.** Taginfo extensions:
   - done: `--combinations` (key and tag co-occurrence);
   - not yet: optional stored `key=*` postings region; substring/n-gram value
     search; per-key "top-N by count" table.
5. **Done** (added after the original plan). Rendering sub-archives (§5a):
   `--multipolygons`, `--coastline`, `--land-polygons`.

---

## 12. Open questions

- **Slot map vs. external sort** for the tag build at planet scale — resident
  `(k,v)→t` hash (simpler, ~hundreds of MB) vs. emit-sort-reduce tuples (more
  I/O, flat memory). *Resolved:* the parent's `tags` are already deduplicated,
  so a slot is just the parent tag index and no slot map is needed; postings
  are count-then-fill with `--mmap-scratch` backing.
- **`key=*` postings** — derive by k-way merge at query time (no storage) vs.
  store a per-key postings region (duplicates refs, ~2× tag postings)?
  *Currently:* derived at query time (`KeyView::{nodes, ways, relations}`).
- **Value ordering within a key** — by string (enables value prefix search) vs.
  by count desc (enables instant "top values"). String wins for search; add a
  small per-key "top-N by count" side table if needed.
- **Fingerprint strength** — counts + schema hash, or a full content hash of the
  parent vectors? Counts+schema is cheap and catches rebuilds; a content hash is
  stronger but costs a full parent read at build. *Currently:* counts +
  stringtable length + replication sequence + schema hash. Note the schema hash
  is of the `osmflat` schema compiled into the binary, not the parent's
  embedded schema bytes (opening a parent with an incompatible schema already
  fails), so it guards reader/writer drift rather than the parent itself.
- **One Ext archive or per-capability archives?** Optional sub-archives under one
  `Ext` (chosen here) vs. fully separate `.taginfo` / `.backrefs` dirs. Sub-
  archives keep one fingerprint/header; separate dirs decouple distribution.
