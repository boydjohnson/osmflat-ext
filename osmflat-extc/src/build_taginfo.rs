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
//! This is the in-RAM form (correct, fine for regional extents). The planet
//! path spills postings to mmap scratch; not wired up here.

use crate::{BuildError, BuildOptions};
use osmflat::Osm;
use osmflat_ext::TaginfoBuilder;
use std::collections::{HashMap, HashSet};

/// Per-type postings for one tag slot, ascending entity indices.
#[derive(Default)]
struct SlotPostings {
    nodes: Vec<u64>,
    ways: Vec<u64>,
    relations: Vec<u64>,
}

/// Build and write the `Taginfo` sub-archive for `parent` into `builder`.
pub fn build(
    parent: &Osm,
    builder: &TaginfoBuilder,
    opts: &BuildOptions,
) -> Result<(), BuildError> {
    let n_tags = parent.tags().len();
    let mut postings: Vec<SlotPostings> = (0..n_tags).map(|_| SlotPostings::default()).collect();

    // Phase 1+2 fused: collect postings per slot, in entity order (ascending).
    collect(parent, |t, entity, ty| match ty {
        EntityType::Node => postings[t].nodes.push(entity),
        EntityType::Way => postings[t].ways.push(entity),
        EntityType::Relation => postings[t].relations.push(entity),
    });

    // Phase 0 (after counting): group slots by key, sort keys/values by string.
    let dict = Dictionary::build(parent);
    let cooccurrences = opts
        .combinations
        .then(|| Cooccurrences::build(parent))
        .unwrap_or_default();

    write(parent, builder, &dict, &postings, &cooccurrences)
}

#[derive(Clone, Copy)]
enum EntityType {
    Node,
    Way,
    Relation,
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
        use std::collections::HashMap;
        let tags = parent.tags();
        let strings = parent.stringtable();

        // Group tag slots by key_idx.
        let mut by_key: HashMap<u64, Vec<u64>> = HashMap::new();
        for (t, tag) in tags.iter().enumerate() {
            by_key.entry(tag.key_idx()).or_default().push(t as u64);
        }

        let mut keys: Vec<KeyGroup> = by_key
            .into_iter()
            .map(|(key_idx, mut slots)| {
                // Sort this key's slots by value string.
                slots.sort_by(|&a, &b| {
                    strings
                        .substring_raw(tags[a as usize].value_idx() as usize)
                        .cmp(strings.substring_raw(tags[b as usize].value_idx() as usize))
                });
                KeyGroup { key_idx, slots }
            })
            .collect();

        // Sort keys by key string.
        keys.sort_by(|a, b| {
            strings
                .substring_raw(a.key_idx as usize)
                .cmp(strings.substring_raw(b.key_idx as usize))
        });

        Dictionary { keys }
    }
}

#[derive(Default)]
struct Cooccurrences {
    by_key: HashMap<u64, Vec<Combo>>,
    by_tag: HashMap<u64, Vec<TagCombo>>,
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
    fn build(parent: &Osm) -> Self {
        let tags = parent.tags();
        let mut key_counts: HashMap<(u64, u64), u64> = HashMap::new();
        let mut tag_counts: HashMap<(u64, u64), u64> = HashMap::new();

        for slots in entity_tag_sets(parent) {
            let mut keys = Vec::with_capacity(slots.len());
            let mut seen_keys = HashSet::with_capacity(slots.len());
            for &slot in &slots {
                let key_idx = tags[slot as usize].key_idx();
                if seen_keys.insert(key_idx) {
                    keys.push(key_idx);
                }
            }

            for &key_idx in &keys {
                for &other_key_idx in &keys {
                    if key_idx != other_key_idx {
                        *key_counts.entry((key_idx, other_key_idx)).or_default() += 1;
                    }
                }
            }

            for &slot in &slots {
                for &other_slot in &slots {
                    if slot != other_slot {
                        *tag_counts.entry((slot, other_slot)).or_default() += 1;
                    }
                }
            }
        }

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

        let mut by_tag: HashMap<u64, Vec<TagCombo>> = HashMap::new();
        for ((slot, other_slot), together_count) in tag_counts {
            by_tag.entry(slot).or_default().push(TagCombo {
                other_slot,
                together_count,
            });
        }

        for combos in by_tag.values_mut() {
            combos.sort_by(|a, b| {
                let a_tag = &tags[a.other_slot as usize];
                let b_tag = &tags[b.other_slot as usize];
                b.together_count
                    .cmp(&a.together_count)
                    .then_with(|| {
                        strings
                            .substring_raw(a_tag.key_idx() as usize)
                            .cmp(strings.substring_raw(b_tag.key_idx() as usize))
                    })
                    .then_with(|| {
                        strings
                            .substring_raw(a_tag.value_idx() as usize)
                            .cmp(strings.substring_raw(b_tag.value_idx() as usize))
                    })
            });
        }

        Self { by_key, by_tag }
    }
}

fn entity_tag_sets(parent: &Osm) -> impl Iterator<Item = Vec<u64>> + '_ {
    let node_tags = parent.nodes().iter().map(|node| node.tags());
    let way_tags = parent.ways().iter().map(|way| way.tags());
    let relation_tags = parent.relations().iter().map(|relation| relation.tags());
    let tags_index = parent.tags_index();

    node_tags
        .chain(way_tags)
        .chain(relation_tags)
        .map(move |range| {
            let mut slots: Vec<u64> = range
                .map(|tag_index_idx| tags_index[tag_index_idx as usize].value())
                .collect();
            slots.sort_unstable();
            slots.dedup();
            slots
        })
}

/// Emit `keys` / `values` / `*_post` in dictionary order, closing each range
/// with a trailing sentinel.
fn write(
    parent: &Osm,
    builder: &TaginfoBuilder,
    dict: &Dictionary,
    postings: &[SlotPostings],
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

    for group in &dict.keys {
        // Per-key aggregate counts (sum of value postings; keys unique per object).
        let (mut cn, mut cw, mut cr) = (0u64, 0u64, 0u64);
        for &t in &group.slots {
            let p = &postings[t as usize];
            cn += p.nodes.len() as u64;
            cw += p.ways.len() as u64;
            cr += p.relations.len() as u64;
        }

        let key = keys_vec.grow()?;
        key.set_key_idx(group.key_idx);
        key.set_count_nodes(cn);
        key.set_count_ways(cw);
        key.set_count_relations(cr);
        key.set_value_first_idx(values_written);
        key.set_combo_first_idx(combos_written);

        for &t in &group.slots {
            let p = &postings[t as usize];
            let value = values_vec.grow()?;
            value.set_value_idx(tags[t as usize].value_idx());
            value.set_node_first_idx(nodes_written);
            value.set_way_first_idx(ways_written);
            value.set_rel_first_idx(rels_written);
            value.set_tag_combo_first_idx(tag_combos_written);

            for &e in &p.nodes {
                node_post.grow()?.set_value(e);
            }
            for &e in &p.ways {
                way_post.grow()?.set_value(e);
            }
            for &e in &p.relations {
                rel_post.grow()?.set_value(e);
            }
            nodes_written += p.nodes.len() as u64;
            ways_written += p.ways.len() as u64;
            rels_written += p.relations.len() as u64;
            values_written += 1;

            if let Some(entries) = combos.by_tag.get(&t) {
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
