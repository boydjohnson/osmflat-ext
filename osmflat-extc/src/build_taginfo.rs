//! Build the `Taginfo` sub-archive: inverted tag index + histograms.
//!
//! The parent's `tags` vector is already deduplicated — each distinct
//! `(key,value)` is exactly one `Tag`, at index `t` — so a tag *slot* is just
//! that `t`. Postings are collected per slot by iterating entities **in index
//! order**, which makes each postings run ascending == spatial (SFC) order with
//! no sort (design §3, §6.2).
//!
//! Then the dictionary is built: group slots by key, sort keys by string and,
//! within a key, values by string; emit `keys` / `values` and lay the postings
//! down grouped by `(key,value)` in that sorted order.
//!
//! The postings are built count-then-fill (design §6.2 phases 1–2): the
//! dictionary fixes each slot's output position, one pass counts occurrences
//! into CSR offsets, a second pass fills the postings array in final layout.
//! Every parent-sized array comes from [`Scratch`], so `--mmap-scratch` backs
//! the build with temp files at planet scale; without it everything stays in
//! RAM (fine for regional extents).
//!
//! The `--combinations` tag-pair co-occurrence counts use a different
//! strategy: a direct CSR fill (write each `(slot, other_slot)` mention at
//! its exact final position) was tried first, but the writes are scattered
//! across the *whole* raw-mentions array — a row's mentions come from
//! entities spread across the entire entity scan, not adjacent in time — so
//! on a country-scale extract the working set thrashes the page cache even
//! though it's disk-backed, not swap. Instead, mentions are radix-bucketed
//! by slot into [`N_TAG_BUCKETS`] sequential append-only streams in one
//! pass (each bucket's writes are purely sequential, the cheapest I/O
//! pattern), then each bucket — small enough to sort comfortably in RAM —
//! is sorted and run-length-encoded independently. The key-pair
//! co-occurrence counts stay in RAM either way, since they're bounded by
//! distinct key-set signatures, not by near-unique per-entity value tags.

use crate::scratch::{Scratch, ScratchU64};
use crate::{BuildError, BuildOptions};
use osmflat::Osm;
use osmflat_ext::TaginfoBuilder;
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// CSR postings for one entity type, slot positions in dictionary order.
struct TypeCsr {
    /// Exclusive prefix sums: position `p`'s postings span
    /// `posts[offsets[p]..offsets[p + 1]]`.
    offsets: ScratchU64,
    /// Ascending entity indices, grouped by slot position.
    posts: ScratchU64,
}

impl TypeCsr {
    fn at(&self, p: usize) -> &[u64] {
        &self.posts[self.offsets[p] as usize..self.offsets[p + 1] as usize]
    }
}

/// Build and write the `Taginfo` sub-archive for `parent` into `builder`.
pub fn build(
    parent: &Osm,
    builder: &TaginfoBuilder,
    opts: &BuildOptions,
) -> Result<(), BuildError> {
    let scratch = Scratch::new(opts.mmap_scratch.as_deref());
    let mut phase_start = std::time::Instant::now();

    // Phase 0: the dictionary fixes the output position of every tag slot.
    let dict = Dictionary::build(parent);
    eprintln!("[taginfo] dictionary build: {:?}", phase_start.elapsed());
    phase_start = std::time::Instant::now();

    let n_slots = parent.tags().len();
    let mut pos = vec![0u64; n_slots];
    for (p, &t) in dict.keys.iter().flat_map(|g| &g.slots).enumerate() {
        pos[t as usize] = p as u64;
    }
    eprintln!("[taginfo] pos vector: {:?}", phase_start.elapsed());
    phase_start = std::time::Instant::now();

    // Phase 1: count occurrences per (position, type), then prefix-sum into
    // CSR offsets so postings land grouped in dictionary order.
    let mut offsets = [
        scratch.alloc(n_slots + 1)?,
        scratch.alloc(n_slots + 1)?,
        scratch.alloc(n_slots + 1)?,
    ];
    collect(parent, |t, _entity, ty| {
        offsets[ty.idx()][pos[t] as usize + 1] += 1;
    });
    eprintln!("[taginfo] phase 1 (count): {:?}", phase_start.elapsed());
    phase_start = std::time::Instant::now();

    for offsets in &mut offsets {
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }
    }
    eprintln!("[taginfo] prefix sum: {:?}", phase_start.elapsed());
    phase_start = std::time::Instant::now();

    // Phase 2: fill. Entity-order iteration makes each run ascending (§3).
    let mut posts = [
        scratch.alloc(offsets[0][n_slots] as usize)?,
        scratch.alloc(offsets[1][n_slots] as usize)?,
        scratch.alloc(offsets[2][n_slots] as usize)?,
    ];
    let mut cursors = [
        scratch.alloc_copy(&offsets[0][..n_slots])?,
        scratch.alloc_copy(&offsets[1][..n_slots])?,
        scratch.alloc_copy(&offsets[2][..n_slots])?,
    ];
    collect(parent, |t, entity, ty| {
        let (i, p) = (ty.idx(), pos[t] as usize);
        posts[i][cursors[i][p] as usize] = entity;
        cursors[i][p] += 1;
    });
    eprintln!("[taginfo] phase 2 (fill): {:?}", phase_start.elapsed());
    drop(cursors);
    drop(pos);

    let [node_offsets, way_offsets, rel_offsets] = offsets;
    let [node_posts, way_posts, rel_posts] = posts;
    let csr = [
        TypeCsr {
            offsets: node_offsets,
            posts: node_posts,
        },
        TypeCsr {
            offsets: way_offsets,
            posts: way_posts,
        },
        TypeCsr {
            offsets: rel_offsets,
            posts: rel_posts,
        },
    ];

    phase_start = std::time::Instant::now();
    let cooccurrences = if opts.combinations {
        Cooccurrences::build(parent, opts.mmap_scratch.as_deref())?
    } else {
        Cooccurrences::default()
    };
    eprintln!("[taginfo] cooccurrences: {:?}", phase_start.elapsed());

    phase_start = std::time::Instant::now();
    let result = write(parent, builder, &dict, &csr, &cooccurrences);
    eprintln!("[taginfo] write: {:?}", phase_start.elapsed());
    result
}

#[derive(Clone, Copy)]
enum EntityType {
    Node,
    Way,
    Relation,
}

impl EntityType {
    fn idx(self) -> usize {
        match self {
            EntityType::Node => 0,
            EntityType::Way => 1,
            EntityType::Relation => 2,
        }
    }
}

/// Visit every `(slot, entity_index, type)` occurrence, entities in index order
/// so postings come out ascending.
fn collect(parent: &Osm, mut visit: impl FnMut(usize, u64, EntityType)) {
    let tags_index = parent.tags_index();
    let slot = |ti: u64| tags_index[ti as usize].value() as usize;
    for (i, node) in parent.nodes().iter().enumerate() {
        for ti in node.tags() {
            visit(slot(ti), i as u64, EntityType::Node);
        }
    }
    for (i, way) in parent.ways().iter().enumerate() {
        for ti in way.tags() {
            visit(slot(ti), i as u64, EntityType::Way);
        }
    }
    for (i, relation) in parent.relations().iter().enumerate() {
        for ti in relation.tags() {
            visit(slot(ti), i as u64, EntityType::Relation);
        }
    }
}

/// Keys sorted by string; within each key, the tag slots sorted by value string.
struct Dictionary {
    /// One entry per distinct key: its `key_idx` and its slots (value-sorted).
    keys: Vec<KeyGroup>,
}

struct KeyGroup {
    key_idx: u64,
    /// Tag slots (`t`) for this key, sorted by value string.
    slots: Vec<u64>,
}

impl Dictionary {
    fn build(parent: &Osm) -> Self {
        let tags = parent.tags();
        let strings = parent.stringtable();

        // Group tag slots by key_idx.
        let mut by_key: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
        for (t, tag) in tags.iter().enumerate() {
            by_key.entry(tag.key_idx()).or_default().push(t as u64);
        }

        let mut keys: Vec<KeyGroup> = by_key
            .into_iter()
            .map(|(key_idx, mut slots)| {
                // Resolve each slot's value string once and sort on the
                // cached slice — `sort_by` re-ran `substring_raw` on every
                // comparison, re-fetching the same string O(log n) times.
                // Tags are deduplicated per (key,value) (module doc), so no
                // two slots in a key group tie on value string.
                let mut decorated: Vec<(&[u8], u64)> = slots
                    .iter()
                    .map(|&t| {
                        (
                            strings.substring_raw(tags[t as usize].value_idx() as usize),
                            t,
                        )
                    })
                    .collect();
                decorated.sort_unstable_by(|a, b| a.0.cmp(b.0));
                slots = decorated.into_iter().map(|(_, t)| t).collect();
                KeyGroup { key_idx, slots }
            })
            .collect();

        // Same decorate-sort-undecorate for the key-string sort.
        let mut decorated: Vec<(&[u8], KeyGroup)> = keys
            .into_iter()
            .map(|group| (strings.substring_raw(group.key_idx as usize), group))
            .collect();
        decorated.sort_unstable_by(|a, b| a.0.cmp(b.0));
        keys = decorated.into_iter().map(|(_, group)| group).collect();

        Dictionary { keys }
    }
}

/// Number of radix buckets tag-pair mentions are partitioned into before
/// sorting. Chosen so each bucket comfortably fits in RAM even on a
/// country-scale extract (~3.6B mentions / 256 buckets * 16 bytes/record ≈
/// 220MB/bucket average); tune up if a single bucket still turns out huge
/// under real skew. File descriptor cost is negligible (macOS default
/// `ulimit -n` is in the hundreds of thousands).
const N_TAG_BUCKETS: usize = 256;

/// Append-only sink for one radix bucket's `(slot, other_slot)` mentions:
/// a growable RAM buffer without `--mmap-scratch`, or a sequential-write
/// temp file with it. Either way, writes are purely append/sequential — no
/// positional seeking — which is what avoids the thrashing a direct CSR
/// fill into one huge array causes (see module docs).
enum BucketSink {
    Ram(Vec<(u64, u64)>),
    File(BufWriter<File>),
}

impl BucketSink {
    fn new(dir: Option<&Path>) -> Result<Self, BuildError> {
        match dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                Ok(BucketSink::File(BufWriter::new(tempfile::tempfile_in(
                    dir,
                )?)))
            }
            None => Ok(BucketSink::Ram(Vec::new())),
        }
    }

    fn push(&mut self, slot: u64, other_slot: u64) -> Result<(), BuildError> {
        match self {
            BucketSink::Ram(v) => v.push((slot, other_slot)),
            BucketSink::File(w) => {
                w.write_all(&slot.to_le_bytes())?;
                w.write_all(&other_slot.to_le_bytes())?;
            }
        }
        Ok(())
    }

    /// Consume the sink and return all `(slot, other_slot)` records.
    fn into_records(self) -> Result<Vec<(u64, u64)>, BuildError> {
        match self {
            BucketSink::Ram(v) => Ok(v),
            BucketSink::File(w) => {
                let mut file = w.into_inner().map_err(|e| e.into_error())?;
                file.seek(SeekFrom::Start(0))?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)?;
                Ok(buf
                    .chunks_exact(16)
                    .map(|c| {
                        (
                            u64::from_le_bytes(c[0..8].try_into().unwrap()),
                            u64::from_le_bytes(c[8..16].try_into().unwrap()),
                        )
                    })
                    .collect())
            }
        }
    }
}

#[derive(Default)]
struct Cooccurrences {
    by_key: HashMap<u64, Vec<Combo>>,
    /// Indexed directly by tag slot (dense, like `pos` above) rather than a
    /// `HashMap`: bounded by `n_slots`, same as the postings CSR.
    by_tag: Vec<Vec<TagCombo>>,
}

struct Combo {
    other_key_idx: u64,
    together_count: u64,
}

struct TagCombo {
    other_slot: u64,
    together_count: u64,
}

impl Cooccurrences {
    fn build(parent: &Osm, mmap_scratch: Option<&Path>) -> Result<Self, BuildError> {
        let tags = parent.tags();
        let n_slots = tags.len();
        let mut phase_start = std::time::Instant::now();

        // OSM tagging is highly repetitive at the *key* level — many
        // entities share an exactly identical key set (e.g. thousands of
        // residential ways all tagged {highway,name,surface,...}) even
        // though their exact tag *values* differ (distinct street names).
        // So grouping by key-set signature and counting each once, scaled
        // by its entity count, is a real win for `key_counts`/`by_key`
        // below, which stay small RAM `FxHashMap`s. The analogous grouping
        // for exact tag-value sets is not: most entities carry a unique
        // `name` tag, so their slot-set signature is close to unique
        // anyway — tried it, and paying to store an owned `Vec<u64>`
        // signature per near-unique entity pushed the system into swap
        // thrashing on a real dataset for no counting savings.
        //
        // The direct per-entity pairwise count has the opposite problem: on
        // a country-scale extract the number of distinct (slot,
        // other_slot) pairs runs into the hundreds of millions to billions
        // (measured on `us.osm.flat`: ~1.7B entities, up to 3.6B raw pair
        // mentions). A direct CSR fill into one array sized to the exact
        // total (like the postings build above) was tried, but each row's
        // mentions come from entities scattered across the *entire* entity
        // scan, so writes hit the whole multi-GB array in an order
        // uncorrelated with physical layout — thrashing the page cache
        // even backed by `--mmap-scratch` (not swap, but the same shape of
        // problem: repeated evict/reload of a working set that doesn't
        // fit). So mentions are radix-bucketed by slot into
        // `N_TAG_BUCKETS` sequential append-only streams instead — see
        // module docs.
        // `get_mut` looks the key-set signature up by borrowed slice, so a
        // repeat (the common case) costs no allocation; only a
        // never-seen-before signature pays for the owned `Vec` key.
        let mut key_set_counts: FxHashMap<Vec<u64>, u64> = FxHashMap::default();
        let mut buckets: Vec<BucketSink> = (0..N_TAG_BUCKETS)
            .map(|_| BucketSink::new(mmap_scratch))
            .collect::<Result<_, BuildError>>()?;
        let bucket_of = |slot: u64| -> usize {
            ((slot * N_TAG_BUCKETS as u64) / n_slots as u64) as usize
        };

        // Reused across entities to avoid an alloc/dealloc per entity.
        let mut keys: Vec<u64> = Vec::new();
        let mut bucket_err = None;

        for_each_entity_tag_set(parent, |slots| {
            if bucket_err.is_some() {
                return;
            }
            keys.clear();
            keys.extend(slots.iter().map(|&slot| tags[slot as usize].key_idx()));
            keys.sort_unstable();
            keys.dedup();

            if let Some(count) = key_set_counts.get_mut(keys.as_slice()) {
                *count += 1;
            } else {
                key_set_counts.insert(keys.clone(), 1);
            }

            for &slot in slots {
                for &other_slot in slots {
                    if slot != other_slot {
                        if let Err(e) = buckets[bucket_of(slot)].push(slot, other_slot) {
                            bucket_err = Some(e);
                            return;
                        }
                    }
                }
            }
        });
        if let Some(e) = bucket_err {
            return Err(e);
        }
        eprintln!(
            "[cooccurrences] group keys + bucket tag pairs: {:?}",
            phase_start.elapsed()
        );
        phase_start = std::time::Instant::now();

        let mut key_counts: FxHashMap<(u64, u64), u64> = FxHashMap::default();
        for (group_keys, &count) in &key_set_counts {
            for &key_idx in group_keys {
                for &other_key_idx in group_keys {
                    if key_idx != other_key_idx {
                        *key_counts.entry((key_idx, other_key_idx)).or_default() += count;
                    }
                }
            }
        }
        eprintln!(
            "[cooccurrences] count key pairs: {:?}",
            phase_start.elapsed()
        );
        phase_start = std::time::Instant::now();

        let strings = parent.stringtable();
        let mut by_key: HashMap<u64, Vec<Combo>> = HashMap::new();
        for ((key_idx, other_key_idx), together_count) in key_counts {
            by_key.entry(key_idx).or_default().push(Combo {
                other_key_idx,
                together_count,
            });
        }

        for combos in by_key.values_mut() {
            combos.sort_by(|a, b| {
                b.together_count.cmp(&a.together_count).then_with(|| {
                    strings
                        .substring_raw(a.other_key_idx as usize)
                        .cmp(strings.substring_raw(b.other_key_idx as usize))
                })
            });
        }
        eprintln!(
            "[cooccurrences] by_key build+sort: {:?}",
            phase_start.elapsed()
        );
        phase_start = std::time::Instant::now();

        // Per bucket: read its mentions back, sort by (slot, other_slot)
        // (cheap integer sort — no string lookups — and cache-friendly
        // since the whole bucket is RAM-resident), then run-length-encode
        // each slot's sub-range into counts and apply the same small
        // decorate-sort-undecorate ordering (count desc, then key/value
        // string) `by_key` uses above. Each slot's *distinct* other-tag
        // count is bounded (unlike its raw mentions before dedup), so this
        // final sort is cheap.
        let mut by_tag: Vec<Vec<TagCombo>> = (0..n_slots).map(|_| Vec::new()).collect();
        for bucket in buckets {
            let mut records = bucket.into_records()?;
            records.sort_unstable();

            let mut i = 0;
            while i < records.len() {
                let slot = records[i].0;
                let mut j = i + 1;
                while j < records.len() && records[j].0 == slot {
                    j += 1;
                }
                let row = &records[i..j];

                let mut decorated: Vec<(u64, &[u8], &[u8], u64)> = Vec::new();
                let mut k = 0;
                while k < row.len() {
                    let other_slot = row[k].1;
                    let mut m = k + 1;
                    while m < row.len() && row[m].1 == other_slot {
                        m += 1;
                    }
                    let other = &tags[other_slot as usize];
                    decorated.push((
                        (m - k) as u64,
                        strings.substring_raw(other.key_idx() as usize),
                        strings.substring_raw(other.value_idx() as usize),
                        other_slot,
                    ));
                    k = m;
                }
                decorated.sort_unstable_by(|a, b| {
                    b.0.cmp(&a.0)
                        .then_with(|| a.1.cmp(b.1))
                        .then_with(|| a.2.cmp(b.2))
                });
                by_tag[slot as usize] = decorated
                    .into_iter()
                    .map(|(together_count, _, _, other_slot)| TagCombo {
                        other_slot,
                        together_count,
                    })
                    .collect();
                i = j;
            }
        }
        eprintln!(
            "[cooccurrences] by_tag bucket sort + RLE: {:?}",
            phase_start.elapsed()
        );

        Ok(Self { by_key, by_tag })
    }
}

/// Visits each entity's deduplicated, sorted tag-slot set in turn. Reuses one
/// buffer across entities instead of allocating a fresh `Vec` per entity.
fn for_each_entity_tag_set(parent: &Osm, mut visit: impl FnMut(&[u64])) {
    let node_tags = parent.nodes().iter().map(|node| node.tags());
    let way_tags = parent.ways().iter().map(|way| way.tags());
    let relation_tags = parent.relations().iter().map(|relation| relation.tags());
    let tags_index = parent.tags_index();

    let mut slots: Vec<u64> = Vec::new();
    for range in node_tags.chain(way_tags).chain(relation_tags) {
        slots.clear();
        slots.extend(range.map(|tag_index_idx| tags_index[tag_index_idx as usize].value()));
        slots.sort_unstable();
        slots.dedup();
        visit(&slots);
    }
}

/// Emit `keys` / `values` / `*_post` in dictionary order, closing each range
/// with a trailing sentinel.
fn write(
    parent: &Osm,
    builder: &TaginfoBuilder,
    dict: &Dictionary,
    csr: &[TypeCsr; 3],
    combos: &Cooccurrences,
) -> Result<(), BuildError> {
    let tags = parent.tags();

    let mut keys_vec = builder.start_keys()?;
    let mut values_vec = builder.start_values()?;
    let mut node_post = builder.start_node_post()?;
    let mut way_post = builder.start_way_post()?;
    let mut rel_post = builder.start_rel_post()?;
    let mut combo_vec = builder.start_combos()?;
    let mut tag_combo_vec = builder.start_tag_combos()?;

    let (mut values_written, mut nodes_written, mut ways_written, mut rels_written) =
        (0u64, 0u64, 0u64, 0u64);
    let mut combos_written = 0u64;
    let mut tag_combos_written = 0u64;

    // Running slot position; the fill laid postings down in dictionary order,
    // so it advances in lockstep with the iteration below.
    let mut p = 0usize;

    for group in &dict.keys {
        // Per-key aggregate counts (sum of value postings; keys unique per object).
        let (mut cn, mut cw, mut cr) = (0u64, 0u64, 0u64);
        for q in p..p + group.slots.len() {
            cn += csr[0].at(q).len() as u64;
            cw += csr[1].at(q).len() as u64;
            cr += csr[2].at(q).len() as u64;
        }

        let key = keys_vec.grow()?;
        key.set_key_idx(group.key_idx);
        key.set_count_nodes(cn);
        key.set_count_ways(cw);
        key.set_count_relations(cr);
        key.set_value_first_idx(values_written);
        key.set_combo_first_idx(combos_written);

        for &t in &group.slots {
            let (nodes, ways, rels) = (csr[0].at(p), csr[1].at(p), csr[2].at(p));
            p += 1;

            let value = values_vec.grow()?;
            value.set_value_idx(tags[t as usize].value_idx());
            value.set_node_first_idx(nodes_written);
            value.set_way_first_idx(ways_written);
            value.set_rel_first_idx(rels_written);
            value.set_tag_combo_first_idx(tag_combos_written);

            for &e in nodes {
                node_post.grow()?.set_value(e);
            }
            for &e in ways {
                way_post.grow()?.set_value(e);
            }
            for &e in rels {
                rel_post.grow()?.set_value(e);
            }
            nodes_written += nodes.len() as u64;
            ways_written += ways.len() as u64;
            rels_written += rels.len() as u64;
            values_written += 1;

            if let Some(entries) = combos.by_tag.get(t as usize) {
                for entry in entries {
                    let other = &tags[entry.other_slot as usize];
                    let combo = tag_combo_vec.grow()?;
                    combo.set_other_key_idx(other.key_idx());
                    combo.set_other_value_idx(other.value_idx());
                    combo.set_together_count(entry.together_count);
                    tag_combos_written += 1;
                }
            }
        }

        if let Some(entries) = combos.by_key.get(&group.key_idx) {
            for entry in entries {
                let combo = combo_vec.grow()?;
                combo.set_other_key_idx(entry.other_key_idx);
                combo.set_together_count(entry.together_count);
                combos_written += 1;
            }
        }
    }

    // Sentinels close the last real key's value range and the last value's
    // postings ranges. flatdata trims these from the reader slices.
    let key_sentinel = keys_vec.grow()?;
    key_sentinel.set_value_first_idx(values_written);
    key_sentinel.set_combo_first_idx(combos_written);
    let value_sentinel = values_vec.grow()?;
    value_sentinel.set_node_first_idx(nodes_written);
    value_sentinel.set_way_first_idx(ways_written);
    value_sentinel.set_rel_first_idx(rels_written);
    value_sentinel.set_tag_combo_first_idx(tag_combos_written);

    keys_vec.close()?;
    values_vec.close()?;
    node_post.close()?;
    way_post.close()?;
    rel_post.close()?;
    combo_vec.close()?;
    tag_combo_vec.close()?;
    Ok(())
}
