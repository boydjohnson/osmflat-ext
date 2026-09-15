//! Inverted tag index + taginfo histogram queries over the [`Taginfo`]
//! sub-archive.
//!
//! Navigation mirrors taginfo.openstreetmap.org: **key → values → objects**.
//! `keys` is sorted by key string (binary-searchable / prefix-scannable);
//! within a key, `values` is sorted by value string; each value carries the
//! three per-type postings ranges. Per-(key,value) per-type counts are *not*
//! stored — they equal the postings range lengths (see [`ValueView`]).
//!
//! All postings are ascending parent indices, i.e. spatial (SFC) order, so they
//! compose with bbox queries and with each other via [`crate::query`].

use crate::query::EntityType;
use crate::{ComboEntry, KeyEntry, Ref, TagComboEntry, Taginfo, ValueEntry};
use osmflat::Osm;

/// The `ValueView` postings accessor for `entity`.
fn value_postings<'a>(entity: EntityType) -> fn(&ValueView<'a>) -> &'a [Ref] {
    match entity {
        EntityType::Node => ValueView::nodes,
        EntityType::Way => ValueView::ways,
        EntityType::Relation => ValueView::relations,
    }
}

/// Object counts split by entity type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TypeCounts {
    pub nodes: u64,
    pub ways: u64,
    pub relations: u64,
}

/// Entry point for taginfo / inverted-index queries.
#[derive(Clone, Copy)]
pub struct TaginfoQuery<'a> {
    parent: &'a Osm,
    taginfo: &'a Taginfo,
}

impl<'a> TaginfoQuery<'a> {
    /// Wrap a parent archive and its `Taginfo` sub-archive. Prefer
    /// [`crate::ExtArchive::taginfo`], which verifies the fingerprint first.
    #[inline]
    pub fn new(parent: &'a Osm, taginfo: &'a Taginfo) -> Self {
        Self { parent, taginfo }
    }

    /// Read the string at `idx` in the **parent** stringtable (NUL-terminated,
    /// returned without the terminator). The unit of all key/value comparisons.
    #[inline]
    fn string(&self, idx: u64) -> &'a [u8] {
        self.parent.stringtable().substring_raw(idx as usize)
    }

    /// Locate a key by exact string via binary search over `keys`.
    /// `O(log K)` plus the comparisons' string reads.
    pub fn key(&self, key: &[u8]) -> Option<KeyView<'a>> {
        let keys = self.taginfo.keys();
        let idx = keys
            .binary_search_by(|k| self.string(k.key_idx()).cmp(key))
            .ok()?;
        Some(KeyView { q: *self, idx })
    }

    /// Keys sharing a string prefix, in sorted order — the taginfo search box.
    /// Binary-search the lower bound, then scan while the prefix holds.
    pub fn keys_with_prefix(&self, prefix: &[u8]) -> impl Iterator<Item = KeyView<'a>> {
        let keys = self.taginfo.keys();
        let lo = keys.partition_point(|k| self.string(k.key_idx()) < prefix);
        let q = *self;
        // Collect the contiguous prefix run; `keys` is sorted, so it ends as
        // soon as the prefix no longer matches.
        let mut out = Vec::new();
        for idx in lo..keys.len() {
            if q.string(keys[idx].key_idx()).starts_with(prefix) {
                out.push(KeyView { q, idx });
            } else {
                break;
            }
        }
        out.into_iter()
    }

    /// All distinct keys, in sorted order (taginfo "keys" table). The flatdata
    /// sentinel is already trimmed from `keys()`, so the whole slice is real.
    pub fn keys(&self) -> impl Iterator<Item = KeyView<'a>> + '_ {
        let n = self.taginfo.keys().len();
        (0..n).map(move |i| KeyView { q: *self, idx: i })
    }

    /// Resolve a `(key, value)` directly to its postings view.
    /// `key(..)` then [`KeyView::value`].
    pub fn kv(&self, key: &[u8], value: &[u8]) -> Option<ValueView<'a>> {
        self.key(key)?.value(value)
    }

    /// Whether the sidecar stores precomputed `key=*` postings
    /// (`osmflat-extc --key-postings`). [`KeyView::nodes`] and friends return
    /// the same result either way; this only affects speed.
    pub fn has_key_postings(&self) -> bool {
        self.taginfo.key_postings().is_some()
    }

    /// Whether the sidecar has the trigram substring index
    /// (`osmflat-extc --value-search`). [`Self::values_containing`] returns the
    /// same result either way; without it, it scans every value.
    pub fn has_value_search(&self) -> bool {
        self.taginfo.value_search().is_some()
    }

    /// Values of any key whose string contains `pattern`, ASCII
    /// case-insensitively (`"lake"` matches `"Lake Harriet"`; non-ASCII bytes
    /// must match exactly). Ascending value index: grouped by key in key order,
    /// value-string order within a key; see [`ValueView::key`].
    ///
    /// With the `--value-search` index and a pattern of at least 3 bytes, the
    /// candidates are the intersection of the pattern's trigram postings, each
    /// then checked. Otherwise every value is checked.
    pub fn values_containing(&self, pattern: &[u8]) -> Vec<ValueView<'a>> {
        self.values_containing_in(0..self.taginfo.values().len(), pattern)
    }

    /// [`Self::values_containing`] restricted to value indices in `range`.
    fn values_containing_in(
        &self,
        range: std::ops::Range<usize>,
        pattern: &[u8],
    ) -> Vec<ValueView<'a>> {
        let values = self.taginfo.values();
        let matches =
            |i: usize| contains_ignore_ascii_case(self.string(values[i].value_idx()), pattern);
        let hits: Vec<usize> = match self.trigram_candidates(pattern) {
            Some(candidates) => {
                let lo = candidates.partition_point(|&v| (v as usize) < range.start);
                let hi = candidates.partition_point(|&v| (v as usize) < range.end);
                candidates[lo..hi]
                    .iter()
                    .map(|&v| v as usize)
                    .filter(|&i| matches(i))
                    .collect()
            }
            None => range.filter(|&i| matches(i)).collect(),
        };
        hits.into_iter()
            .map(|idx| ValueView { q: *self, idx })
            .collect()
    }

    /// Ascending value indices containing every trigram of `pattern`, or
    /// `None` when there's no index or the pattern is too short to use it.
    fn trigram_candidates(&self, pattern: &[u8]) -> Option<Vec<u64>> {
        if pattern.len() < 3 {
            return None;
        }
        let search = self.taginfo.value_search()?;
        let (trigrams, postings) = (search.trigrams(), search.values());
        let mut grams = Vec::new();
        value_trigrams(pattern, &mut grams);

        let mut lists: Vec<&[Ref]> = Vec::with_capacity(grams.len());
        for gram in grams {
            let Ok(i) = trigrams.binary_search_by_key(&gram, |t| t.gram()) else {
                return Some(Vec::new());
            };
            let r = trigrams[i].values();
            lists.push(&postings[r.start as usize..r.end as usize]);
        }
        lists.sort_by_key(|l| l.len());
        let mut acc: Vec<u64> = lists[0].iter().map(|r| r.value()).collect();
        for list in &lists[1..] {
            if acc.is_empty() {
                break;
            }
            acc = crate::query::intersect_sorted(&acc, list.iter().map(|r| r.value()));
        }
        Some(acc)
    }
}

/// The distinct trigrams of `value` as indexed by the `ValueSearch`
/// sub-archive: every 3-byte window, ASCII-lowercased, encoded
/// `b0 << 16 | b1 << 8 | b2`, sorted and deduplicated into `out`.
///
/// `osmflat-extc` builds the index with this function, so readers and the
/// builder always agree.
pub fn value_trigrams(value: &[u8], out: &mut Vec<u32>) {
    out.clear();
    out.extend(value.windows(3).map(|w| {
        u32::from(w[0].to_ascii_lowercase()) << 16
            | u32::from(w[1].to_ascii_lowercase()) << 8
            | u32::from(w[2].to_ascii_lowercase())
    }));
    out.sort_unstable();
    out.dedup();
}

/// Whether `haystack` contains `needle`, comparing ASCII letters
/// case-insensitively.
fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}

/// A distinct key with its aggregate stats and value list.
#[derive(Clone, Copy)]
pub struct KeyView<'a> {
    q: TaginfoQuery<'a>,
    idx: usize,
}

impl<'a> KeyView<'a> {
    #[inline]
    fn entry(&self) -> &'a KeyEntry {
        &self.q.taginfo.keys()[self.idx]
    }

    /// The key string.
    #[inline]
    pub fn key(&self) -> &'a [u8] {
        self.q.string(self.entry().key_idx())
    }

    /// Per-type object counts carrying this key (`O(1)`).
    #[inline]
    pub fn counts(&self) -> TypeCounts {
        let e = self.entry();
        TypeCounts {
            nodes: e.count_nodes(),
            ways: e.count_ways(),
            relations: e.count_relations(),
        }
    }

    /// Number of distinct values for this key (`O(1)` from the value range).
    pub fn distinct_values(&self) -> u64 {
        let r = self.entry().values();
        r.end - r.start
    }

    /// Other keys used by entities that carry this key.
    ///
    /// Entries are sorted by descending together-count, then by key string.
    /// Empty when the sidecar was built without `--combinations`.
    pub fn combinations(&self) -> impl Iterator<Item = CombinationView<'a>> + '_ {
        let r = self.entry().combos();
        let q = self.q;
        (r.start..r.end).map(move |i| CombinationView { q, idx: i as usize })
    }

    /// The distinct values for this key, sorted by value string.
    pub fn values(&self) -> impl Iterator<Item = ValueView<'a>> + '_ {
        let r = self.entry().values();
        let q = self.q;
        (r.start..r.end).map(move |i| ValueView { q, idx: i as usize })
    }

    /// The distinct values for this key, most common first: by total object
    /// count (nodes + ways + relations) descending, then by value string.
    pub fn values_by_count(&self) -> impl Iterator<Item = ValueView<'a>> + 'a {
        let r = self.entry().values();
        let q = self.q;
        q.taginfo.values_by_count()[r.start as usize..r.end as usize]
            .iter()
            .map(move |v| ValueView {
                q,
                idx: v.value() as usize,
            })
    }

    /// This key's values whose string contains `pattern`, ASCII
    /// case-insensitively, in value-string order. See
    /// [`TaginfoQuery::values_containing`].
    pub fn values_containing(&self, pattern: &[u8]) -> Vec<ValueView<'a>> {
        let r = self.entry().values();
        self.q
            .values_containing_in(r.start as usize..r.end as usize, pattern)
    }

    /// Nodes carrying this key with any value (`key=*`), ascending parent node
    /// indices.
    ///
    /// Read directly from the stored key postings when the sidecar was built
    /// with `--key-postings`; otherwise a lazy k-way merge
    /// ([`crate::query::union`]) over this key's per-value node postings, in
    /// `O(values)` memory. Both give the same result.
    pub fn nodes(&self) -> impl Iterator<Item = u64> + 'a {
        self.postings_of(EntityType::Node)
    }

    /// Ways carrying this key with any value (`key=*`). See [`Self::nodes`].
    pub fn ways(&self) -> impl Iterator<Item = u64> + 'a {
        self.postings_of(EntityType::Way)
    }

    /// Relations carrying this key with any value (`key=*`). See [`Self::nodes`].
    pub fn relations(&self) -> impl Iterator<Item = u64> + 'a {
        self.postings_of(EntityType::Relation)
    }

    /// Nodes carrying this key with any value that fall in `bbox`, ascending.
    ///
    /// Each value's postings are clipped to the bbox's entity-index ranges by
    /// leapfrogging (binary-searching past gaps), then merged, so a small bbox
    /// doesn't scan every posting of a large key.
    pub fn nodes_in_bbox(&self, bbox: crate::query::Bbox) -> Vec<u64> {
        let idx = crate::query::node_indices_in_bbox(self.q.parent, bbox);
        self.postings_within(EntityType::Node, &crate::query::to_index_ranges(&idx))
    }

    /// Ways carrying this key with any value that overlap `bbox`. See
    /// [`Self::nodes_in_bbox`].
    pub fn ways_in_bbox(&self, bbox: crate::query::Bbox) -> Vec<u64> {
        let idx = crate::query::way_indices_in_bbox(self.q.parent, bbox);
        self.postings_within(EntityType::Way, &crate::query::to_index_ranges(&idx))
    }

    /// Relations carrying this key with any value that overlap `bbox`. See
    /// [`Self::nodes_in_bbox`].
    pub fn relations_in_bbox(&self, bbox: crate::query::Bbox) -> Vec<u64> {
        let idx = crate::query::relation_indices_in_bbox(self.q.parent, bbox);
        self.postings_within(EntityType::Relation, &crate::query::to_index_ranges(&idx))
    }

    /// `key=*` for `entity`, ascending: stored postings if present, else the
    /// merge of this key's value postings.
    pub(crate) fn postings_of(&self, entity: EntityType) -> impl Iterator<Item = u64> + 'a {
        let stored = self.stored_postings(entity);
        let merged = match stored {
            Some(_) => None,
            None => Some(crate::query::union(&self.postings(value_postings(entity)))),
        };
        stored
            .into_iter()
            .flatten()
            .map(|r| r.value())
            .chain(merged.into_iter().flatten())
    }

    /// `key=*` for `entity`, restricted to the ascending, disjoint `ranges`.
    pub(crate) fn postings_within(
        &self,
        entity: EntityType,
        ranges: &[std::ops::Range<u64>],
    ) -> Vec<u64> {
        if let Some(stored) = self.stored_postings(entity) {
            return crate::query::clip_postings(stored, ranges).collect();
        }
        let clipped = self
            .postings(value_postings(entity))
            .into_iter()
            .map(|postings| crate::query::clip_postings(postings, ranges))
            .collect();
        crate::query::union_iters(clipped).collect()
    }

    /// This key's precomputed `key=*` postings for `entity`, if the sidecar
    /// was built with `--key-postings`.
    fn stored_postings(&self, entity: EntityType) -> Option<&'a [Ref]> {
        let kp = self.q.taginfo.key_postings()?;
        let (ranges, posts) = match entity {
            EntityType::Node => (kp.node_range(), kp.nodes()),
            EntityType::Way => (kp.way_range(), kp.ways()),
            EntityType::Relation => (kp.relation_range(), kp.relations()),
        };
        let r = ranges.get(self.idx)?.post();
        posts.get(r.start as usize..r.end as usize)
    }

    /// One postings slice per value of this key, for the given entity type.
    fn postings(&self, of_type: fn(&ValueView<'a>) -> &'a [Ref]) -> Vec<&'a [Ref]> {
        let r = self.entry().values();
        let q = self.q;
        (r.start..r.end)
            .map(|i| of_type(&ValueView { q, idx: i as usize }))
            .collect()
    }

    /// Find one value of this key by exact string (binary search within the
    /// key's value range).
    pub fn value(&self, value: &[u8]) -> Option<ValueView<'a>> {
        let r = self.entry().values();
        let q = self.q;
        let slice = &q.taginfo.values()[r.start as usize..r.end as usize];
        let off = slice
            .binary_search_by(|v| q.string(v.value_idx()).cmp(value))
            .ok()?;
        Some(ValueView {
            q,
            idx: r.start as usize + off,
        })
    }
}

/// A co-occurring key for a [`KeyView`].
#[derive(Clone, Copy)]
pub struct CombinationView<'a> {
    q: TaginfoQuery<'a>,
    idx: usize,
}

impl<'a> CombinationView<'a> {
    #[inline]
    fn entry(&self) -> &'a ComboEntry {
        &self.q.taginfo.combos()[self.idx]
    }

    /// The co-occurring key string.
    #[inline]
    pub fn key(&self) -> &'a [u8] {
        self.q.string(self.entry().other_key_idx())
    }

    /// Number of parent entities that carry both keys.
    #[inline]
    pub fn together_count(&self) -> u64 {
        self.entry().together_count()
    }
}

/// A co-occurring tag for a [`ValueView`].
#[derive(Clone, Copy)]
pub struct TagCombinationView<'a> {
    q: TaginfoQuery<'a>,
    idx: usize,
}

impl<'a> TagCombinationView<'a> {
    #[inline]
    fn entry(&self) -> &'a TagComboEntry {
        &self.q.taginfo.tag_combos()[self.idx]
    }

    /// The co-occurring key string.
    #[inline]
    pub fn key(&self) -> &'a [u8] {
        self.q.string(self.entry().other_key_idx())
    }

    /// The co-occurring value string.
    #[inline]
    pub fn value(&self) -> &'a [u8] {
        self.q.string(self.entry().other_value_idx())
    }

    /// Number of parent entities that carry both tags.
    #[inline]
    pub fn together_count(&self) -> u64 {
        self.entry().together_count()
    }
}

/// A `(key, value)` with its per-type postings.
#[derive(Clone, Copy)]
pub struct ValueView<'a> {
    q: TaginfoQuery<'a>,
    idx: usize,
}

impl<'a> ValueView<'a> {
    #[inline]
    fn entry(&self) -> &'a ValueEntry {
        &self.q.taginfo.values()[self.idx]
    }

    /// The value string.
    #[inline]
    pub fn value(&self) -> &'a [u8] {
        self.q.string(self.entry().value_idx())
    }

    /// This value's index in the `Taginfo.values` vector.
    #[inline]
    pub fn index(&self) -> usize {
        self.idx
    }

    /// The key this value belongs to (binary search over the keys' value
    /// ranges, `O(log K)`).
    pub fn key(&self) -> KeyView<'a> {
        let keys = self.q.taginfo.keys();
        let idx = keys.partition_point(|k| k.values().end as usize <= self.idx);
        KeyView { q: self.q, idx }
    }

    /// Per-type counts, derived from postings range lengths (`O(1)`).
    pub fn counts(&self) -> TypeCounts {
        TypeCounts {
            nodes: self.nodes().len() as u64,
            ways: self.ways().len() as u64,
            relations: self.relations().len() as u64,
        }
    }

    /// Other tags used by entities that carry this exact `(key,value)`.
    ///
    /// Entries are sorted by descending together-count, then by key string and
    /// value string. Empty when the sidecar was built without `--combinations`.
    pub fn combinations(&self) -> impl Iterator<Item = TagCombinationView<'a>> + '_ {
        let r = self.entry().tag_combos();
        let q = self.q;
        (r.start..r.end).map(move |i| TagCombinationView { q, idx: i as usize })
    }

    /// Node postings (ascending parent node indices).
    #[inline]
    pub fn nodes(&self) -> &'a [Ref] {
        let r = self.entry().node_post();
        &self.q.taginfo.node_post()[r.start as usize..r.end as usize]
    }

    /// Way postings (ascending parent way indices).
    #[inline]
    pub fn ways(&self) -> &'a [Ref] {
        let r = self.entry().way_post();
        &self.q.taginfo.way_post()[r.start as usize..r.end as usize]
    }

    /// Relation postings (ascending parent relation indices).
    #[inline]
    pub fn relations(&self) -> &'a [Ref] {
        let r = self.entry().rel_post();
        &self.q.taginfo.rel_post()[r.start as usize..r.end as usize]
    }

    /// Nodes with this `(key,value)` that fall in `bbox` — the
    /// `key=value ∩ bbox` merge-join. Ascending node indices.
    ///
    /// Sources the bbox candidate ranges from osmflat's exact spatial query,
    /// then runs the `O(R·log k)` range merge-join ([`crate::query::intersect_bbox`])
    /// against this value's node postings.
    pub fn nodes_in_bbox(&self, bbox: crate::query::Bbox) -> Vec<u64> {
        let idx = crate::query::node_indices_in_bbox(self.q.parent, bbox);
        let ranges = crate::query::to_index_ranges(&idx);
        crate::query::intersect_bbox(self.nodes(), &ranges).collect()
    }

    /// Ways with this `(key,value)` overlapping `bbox`. See [`Self::nodes_in_bbox`].
    pub fn ways_in_bbox(&self, bbox: crate::query::Bbox) -> Vec<u64> {
        let idx = crate::query::way_indices_in_bbox(self.q.parent, bbox);
        let ranges = crate::query::to_index_ranges(&idx);
        crate::query::intersect_bbox(self.ways(), &ranges).collect()
    }

    /// Relations with this `(key,value)` overlapping `bbox`. See [`Self::nodes_in_bbox`].
    pub fn relations_in_bbox(&self, bbox: crate::query::Bbox) -> Vec<u64> {
        let idx = crate::query::relation_indices_in_bbox(self.q.parent, bbox);
        let ranges = crate::query::to_index_ranges(&idx);
        crate::query::intersect_bbox(self.relations(), &ranges).collect()
    }
}
