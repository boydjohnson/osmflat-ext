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

use crate::{KeyEntry, Ref, Taginfo, ValueEntry};
use osmflat::Osm;

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

    /// The distinct values for this key, sorted by value string.
    pub fn values(&self) -> impl Iterator<Item = ValueView<'a>> + '_ {
        let r = self.entry().values();
        let q = self.q;
        (r.start..r.end).map(move |i| ValueView { q, idx: i as usize })
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

    /// Per-type counts, derived from postings range lengths (`O(1)`).
    pub fn counts(&self) -> TypeCounts {
        TypeCounts {
            nodes: self.nodes().len() as u64,
            ways: self.ways().len() as u64,
            relations: self.relations().len() as u64,
        }
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
