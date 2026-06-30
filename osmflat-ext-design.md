# Design: `osmflat-ext` — sidecar indexes & query support for osmflat archives

Status: draft design doc (NOT in the repo). Target branch context:
`feature/spatial-index`, where **array order is the spatial index** (nodes by
`ZCurve2D`, ways/relations by `XZ2SFC` of their bounding box) and the optional
`Ids` sub-archive carries OSM ids plus `*_by_id` permutations for reverse
(OSM id → index) lookup.

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
    /// Fingerprint of the exact parent archive this sidecar indexes (§8).
    header: ExtHeader;

    /// Inverted tag index + taginfo histograms (§4).
    @optional taginfo: archive Taginfo;

    /// Reverse references (§5).
    @optional backrefs: archive Backrefs;
}
```

Each capability is an **optional sub-archive**, mirroring `@optional ids:
archive Ids` in the parent: build only what you need (`--taginfo`,
`--backrefs`).

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

// ...also needs its own stringtable for builder_idx
```

The query layer opens parent + sidecar together and asserts the fingerprint
before serving any query. Mismatch → hard error ("ext archive built against a
different parent; rebuild").

---

## 3. The lever: the inverted index composes with the spatial index for free

On `feature/spatial-index`, **entity index order *is* SFC order**. If tag
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
feature and the reason to build the inverted index on this branch specifically.
It generalizes:

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
key's value postings; each operand is ascending, so it streams. If `key=*`
turns out hot, add an optional fourth postings region per `KeyEntry`
(key-level postings) at the cost of duplicating refs.

### Taginfo "combinations" (phase 2)

taginfo's "other keys used by objects that have this key" is a sparse key×key
co-occurrence matrix. Building it means, per entity, enumerating its key-pairs
and incrementing — heavier than the linear passes above. Defer to phase 2 as an
optional addition:

```
struct ComboEntry { other_key_idx: u64 : 40; together_count: u64 : 40; }
// add to KeyEntry:  @range(combos) combo_first_idx: u64 : 40;
// add to archive:   combos: vector<ComboEntry>;   // per key, sorted by count desc
```

Build via an external "emit (key_i,key_j,entity) pairs → sort → reduce" pass
(see §6's planet strategy); v1 of `--taginfo` ships without it.

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
let ext = osmflat_ext::Ext::open(sidecar, &osm)?;   // verifies §8 fingerprint

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

### Query-side non-bbox spatial (item 4) — no sidecar

Built on the existing SFC order; pure lib code:

- **radius / k-NN** — seed with the cell containing the point, expand bbox in
  rings, refine by true distance, stop when the k-th best is closer than the
  ring boundary. Reuses `find_*_by_bounding_box`.
- **polygon** — bbox-of-polygon prefilter via the spatial query, then exact
  point-in-polygon (nodes) / bbox-overlap-then-segment test (ways) refine.

These compose with tag filters through the same ascending-run merge: a
spatial candidate set is a set of index ranges; intersect with tag postings.

---

## 8. Crate / binary structure

Mirror the `osmflat` (lib) / `osmflatc` (lib+bin) split:

```
osmflat-ext        (lib)  flatdata reader bindings for the Ext archive
                          + query API (§7) + non-bbox spatial + id-lookup helper
osmflat-extc       (bin)  the compiler (§6); subcommands --taginfo --backrefs
```

`osmflat-extc` depends on `osmflat` for the reader and reuses nothing of the
parent's writer except the flatdata builder for the sidecar. CLI sketch:

```
osmflat-extc [--out DIR] [--mmap-scratch DIR] [--taginfo] [--backrefs] PARENT
  PARENT           input osmflat archive dir
  --out DIR        output Ext archive dir (default: PARENT sibling .ext)
  --taginfo        build the Taginfo sub-archive
  --backrefs       build the Backrefs sub-archive
  --combinations   also build taginfo co-occurrence (phase 2; implies --taginfo)
  --mmap-scratch   back postings build with mmap temp files here (planet scale)
```

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

---

## 10. Known limitations (state in `--help` / README)

1. **Sidecar is parent-bound.** Stores parent indices; invalid against any
   other/rebuilt parent (caught by §8 fingerprint, not silently).
2. **Build cost ≈ data size.** Postings are O(tags_index); planet needs scratch
   disk, like osmflatc's RocksDB phase.
3. **Substring value search not indexed.** `keys`/`values` are prefix-searchable
   (string-sorted); arbitrary substring search over ~10⁸ planet values is a scan
   or a future n-gram sidecar.
4. **Combinations are phase 2.** v1 `--taginfo` has no co-occurrence table.
5. **No geometry stored.** Spatial refinement (radius/polygon) recomputes from
   the parent, inheriting the way-bbox recompute cost noted in the spatial
   summary.

---

## 11. Phased roadmap

1. `Taginfo` schema + compiler (`--taginfo`, phases 0–2) + query API +
   `k=v ∩ bbox` merge-join. **(the taginfo.openstreetmap core)**
2. `osmflat-ext` query lib polish: boolean tag composition, id-lookup helper
   over the parent `Ids` permutation, non-bbox spatial (radius/k-NN/polygon).
3. `Backrefs` (`--backrefs`).
4. Taginfo `--combinations`; optional `key=*` postings region; substring/n-gram
   value search.

---

## 12. Open questions

- **Slot map vs. external sort** for the tag build at planet scale — resident
  `(k,v)→t` hash (simpler, ~hundreds of MB) vs. emit-sort-reduce tuples (more
  I/O, flat memory). Default to resident with `--mmap-scratch` fallback?
- **`key=*` postings** — derive by k-way merge at query time (no storage) vs.
  store a per-key postings region (duplicates refs, ~2× tag postings)?
- **Value ordering within a key** — by string (enables value prefix search) vs.
  by count desc (enables instant "top values"). String wins for search; add a
  small per-key "top-N by count" side table if needed.
- **Fingerprint strength** — counts + schema hash, or a full content hash of the
  parent vectors? Counts+schema is cheap and catches rebuilds; a content hash is
  stronger but costs a full parent read at build.
- **One Ext archive or per-capability archives?** Optional sub-archives under one
  `Ext` (chosen here) vs. fully separate `.taginfo` / `.backrefs` dirs. Sub-
  archives keep one fingerprint/header; separate dirs decouple distribution.
