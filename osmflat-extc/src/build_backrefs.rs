//! Build the `Backrefs` sub-archive: node->ways and X->relations reverse refs.
//!
//! Two CSR count-then-fill builds (`osmflat-ext-design.md` §6.3):
//!
//! * **node -> ways** from `ways[*].refs() -> nodes_index`: each way ref
//!   contributes a (node, way) backref.
//! * **X -> relations** from `relation_members`: each `NodeMember` /
//!   `WayMember` / `RelationMember` contributes a (member, relation) backref.
//!
//! Like the tag index, filling in ascending parent order yields ascending
//! (spatially-ordered) postings, so backref results compose with bbox and tag
//! selections. No dictionary phase: postings are keyed directly on parent index.

use crate::BuildError;
use osmflat::Osm;
use osmflat_ext::BackrefsBuilder;

/// Build and write the `Backrefs` sub-archive for `parent` into `builder`.
pub fn build(_parent: &Osm, _builder: &BackrefsBuilder) -> Result<(), BuildError> {
    // node -> ways
    build_node_ways(_parent, _builder)?;
    // node/way/relation -> relations
    build_relation_membership(_parent, _builder)?;
    Ok(())
}

/// CSR build of the node->ways reverse index from each way's node refs.
fn build_node_ways(_parent: &Osm, _builder: &BackrefsBuilder) -> Result<(), BuildError> {
    todo!("count refs per node -> prefix-sum node_ways_range -> fill ways_of_node (ascending)")
}

/// CSR build of the membership reverse indexes from `relation_members`.
fn build_relation_membership(_parent: &Osm, _builder: &BackrefsBuilder) -> Result<(), BuildError> {
    todo!(
        "iterate relation_members; per variant bump node/way/rel counters; \
         prefix-sum the three *_rels_range; fill rels_of_node/way/rel (ascending)"
    )
}
