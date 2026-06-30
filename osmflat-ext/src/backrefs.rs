//! Reverse-reference queries over the [`Backrefs`] sub-archive.
//!
//! Answers "which ways use node X" and "which relations contain X" for every
//! member type. Each `*_range` vector is parallel to a parent entity vector
//! (plus a trailing sentinel); element `i`'s [`Range`](crate::Range) slices the
//! matching postings vector. Postings are ascending parent indices, so results
//! compose with bbox and tag selections through [`crate::query`].

use crate::{Backrefs, Ref};
use osmflat::Osm;

/// Entry point for reverse-reference queries.
#[derive(Clone, Copy)]
pub struct BackrefsQuery<'a> {
    #[allow(dead_code)]
    parent: &'a Osm,
    backrefs: &'a Backrefs,
}

impl<'a> BackrefsQuery<'a> {
    /// Wrap a parent archive and its `Backrefs` sub-archive. Prefer
    /// [`crate::ExtArchive::backrefs`], which verifies the fingerprint first.
    #[inline]
    pub fn new(parent: &'a Osm, backrefs: &'a Backrefs) -> Self {
        Self { parent, backrefs }
    }

    /// Ways that reference the node at parent index `node_idx` (ascending).
    pub fn ways_using_node(&self, node_idx: usize) -> &'a [Ref] {
        slice_range(
            self.backrefs.node_ways_range(),
            self.backrefs.ways_of_node(),
            node_idx,
        )
    }

    /// Relations whose members include the node at `node_idx` (ascending).
    pub fn relations_with_node(&self, node_idx: usize) -> &'a [Ref] {
        slice_range(
            self.backrefs.node_rels_range(),
            self.backrefs.rels_of_node(),
            node_idx,
        )
    }

    /// Relations whose members include the way at `way_idx` (ascending).
    pub fn relations_with_way(&self, way_idx: usize) -> &'a [Ref] {
        slice_range(
            self.backrefs.way_rels_range(),
            self.backrefs.rels_of_way(),
            way_idx,
        )
    }

    /// Relations whose members include the relation at `relation_idx`.
    pub fn relations_with_relation(&self, relation_idx: usize) -> &'a [Ref] {
        slice_range(
            self.backrefs.rel_rels_range(),
            self.backrefs.rels_of_rel(),
            relation_idx,
        )
    }
}

/// Slice `postings` by the `@range` stored at `ranges[idx]`. The generated
/// `Range::post()` reads the *next* element's `first_idx`, so a trailing
/// sentinel must close the last real range.
#[inline]
fn slice_range<'a>(_ranges: &'a [crate::Range], _postings: &'a [Ref], _idx: usize) -> &'a [Ref] {
    todo!("let r = ranges[idx].post(); &postings[r.start as usize..r.end as usize]")
}
