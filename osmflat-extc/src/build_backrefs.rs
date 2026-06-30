//! Build the `Backrefs` sub-archive: node->ways and X->relations reverse refs.
//!
//! Two reverse maps (design §5, §6.3):
//!
//! * **node -> ways** from `ways[*].refs() -> nodes_index`: each resolvable way
//!   ref contributes a (node, way) backref.
//! * **X -> relations** from `relation_members`: each `NodeMember` /
//!   `WayMember` / `RelationMember` contributes a (member, relation) backref.
//!
//! Iterating parents (ways, then relations) **in index order** makes each
//! postings run ascending == spatial (SFC) order, so backref results compose
//! with bbox and tag selections. A parent that references the same child twice
//! (a closed way's shared endpoint, a member listed twice) yields one backref:
//! the duplicates are consecutive within a single parent's iteration, so a
//! last-element check dedups them while preserving order.
//!
//! In-RAM form (correct, fine for regional extents); planet spills the postings
//! to mmap scratch — not wired up here.

use crate::BuildError;
use osmflat::{Osm, RelationMembersRef};
use osmflat_ext::{BackrefsBuilder, Range, Ref};

/// Build and write the `Backrefs` sub-archive for `parent` into `builder`.
pub fn build(parent: &Osm, builder: &BackrefsBuilder) -> Result<(), BuildError> {
    let n_nodes = parent.nodes().len();
    let n_ways = parent.ways().len();
    let n_rels = parent.relations().len();

    let mut node_ways: Vec<Vec<u64>> = vec![Vec::new(); n_nodes];
    let mut rels_of_node: Vec<Vec<u64>> = vec![Vec::new(); n_nodes];
    let mut rels_of_way: Vec<Vec<u64>> = vec![Vec::new(); n_ways];
    let mut rels_of_rel: Vec<Vec<u64>> = vec![Vec::new(); n_rels];

    // node -> ways, from each way's resolved node refs.
    let nodes_index = parent.nodes_index();
    for (w, way) in parent.ways().iter().enumerate() {
        for ri in way.refs() {
            if let Some(n) = nodes_index[ri as usize].value() {
                push_distinct(&mut node_ways[n as usize], w as u64);
            }
        }
    }

    // node/way/relation -> relations, from each relation's members.
    let members = parent.relation_members();
    for r in 0..n_rels {
        for member in members.at(r) {
            match member {
                RelationMembersRef::NodeMember(m) => {
                    if let Some(n) = m.node_idx() {
                        push_distinct(&mut rels_of_node[n as usize], r as u64);
                    }
                }
                RelationMembersRef::WayMember(m) => {
                    if let Some(w) = m.way_idx() {
                        push_distinct(&mut rels_of_way[w as usize], r as u64);
                    }
                }
                RelationMembersRef::RelationMember(m) => {
                    if let Some(rr) = m.relation_idx() {
                        push_distinct(&mut rels_of_rel[rr as usize], r as u64);
                    }
                }
            }
        }
    }

    write_csr(
        builder.start_node_ways_range()?,
        builder.start_ways_of_node()?,
        &node_ways,
    )?;
    write_csr(
        builder.start_node_rels_range()?,
        builder.start_rels_of_node()?,
        &rels_of_node,
    )?;
    write_csr(
        builder.start_way_rels_range()?,
        builder.start_rels_of_way()?,
        &rels_of_way,
    )?;
    write_csr(
        builder.start_rel_rels_range()?,
        builder.start_rels_of_rel()?,
        &rels_of_rel,
    )?;
    Ok(())
}

/// Append `x` unless it is already the last element (dedups consecutive repeats
/// while keeping ascending order).
#[inline]
fn push_distinct(v: &mut Vec<u64>, x: u64) {
    if v.last() != Some(&x) {
        v.push(x);
    }
}

/// Write a CSR pair: one `Range` per parent entity (plus a trailing sentinel
/// flatdata trims on read) whose `post()` slices the flattened `posts` vector.
fn write_csr(
    mut ranges: flatdata::ExternalVector<Range>,
    mut posts: flatdata::ExternalVector<Ref>,
    lists: &[Vec<u64>],
) -> Result<(), BuildError> {
    let mut written = 0u64;
    for list in lists {
        ranges.grow()?.set_first_idx(written);
        for &v in list {
            posts.grow()?.set_value(v);
            written += 1;
        }
    }
    ranges.grow()?.set_first_idx(written); // sentinel closes the last range
    ranges.close()?;
    posts.close()?;
    Ok(())
}
