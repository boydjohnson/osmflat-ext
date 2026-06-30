//! Test helpers for building synthetic parent archives and matching extension
//! sidecars without relying on archive directories on disk.

use crate::{build_into, BuildError, BuildOptions};
use flatdata::MemoryResourceStorage;
use osmflat::{
    bbox_index, node_curve, node_index, way_curve, Header, NodeIndex, Osm, OsmBuilder, Tag,
    TagIndex, RELATION_NO_BBOX,
};
use std::collections::HashMap;

pub use osmflat::test_support::{scale, unscale, COORD_SCALE};

/// A synthetic parent archive fixture.
#[derive(Clone, Debug, Default)]
pub struct Fixture {
    pub nodes: Vec<NodeSpec>,
    pub ways: Vec<WaySpec>,
    pub relations: Vec<RelationSpec>,
}

/// A node in a synthetic parent archive.
#[derive(Clone, Debug)]
pub struct NodeSpec {
    pub lon: f64,
    pub lat: f64,
    pub tags: Vec<TagSpec>,
}

/// A way in a synthetic parent archive.
#[derive(Clone, Debug, Default)]
pub struct WaySpec {
    /// Indices into [`Fixture::nodes`].
    pub refs: Vec<usize>,
    pub tags: Vec<TagSpec>,
}

/// A relation in a synthetic parent archive.
#[derive(Clone, Debug, Default)]
pub struct RelationSpec {
    pub bbox: Option<(f64, f64, f64, f64)>,
    pub members: Vec<MemberSpec>,
    pub tags: Vec<TagSpec>,
}

/// A relation member, addressed by indices into the unsorted [`Fixture`].
#[derive(Clone, Debug)]
pub enum MemberSpec {
    Node(usize),
    Way(usize),
    Relation(usize),
}

/// A key/value tag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagSpec {
    pub key: &'static str,
    pub value: &'static str,
}

impl TagSpec {
    pub fn new(key: &'static str, value: &'static str) -> Self {
        Self { key, value }
    }
}

/// Build an in-memory extension archive for an already-open parent.
pub fn build_ext(parent: &Osm, opts: &BuildOptions) -> Result<osmflat_ext::Ext, BuildError> {
    let storage = MemoryResourceStorage::new("/osmflat-ext-test");
    build_into(parent, storage.clone(), opts)?;
    Ok(osmflat_ext::Ext::open(storage)?)
}

/// Build and verify an in-memory extension archive for `parent`.
pub fn build_ext_archive(
    parent: Osm,
    opts: &BuildOptions,
) -> Result<osmflat_ext::ExtArchive, BuildError> {
    let ext = build_ext(&parent, opts)?;
    Ok(osmflat_ext::ExtArchive::open(parent, ext)
        .expect("extension built from this parent must match its fingerprint"))
}

/// Build an in-memory `Osm` archive from a fixture. Entities are emitted in the
/// same spatial order expected by osmflat's bbox query functions.
pub fn build_parent_archive(fixture: &Fixture) -> Result<Osm, BuildError> {
    let storage = MemoryResourceStorage::new("/osmflat-parent-test");
    let builder = OsmBuilder::new(storage.clone())?;

    let mut header = Header::new();
    header.set_coord_scale(COORD_SCALE);
    builder.set_header(&header)?;

    let node_order = node_order(fixture);
    let final_node_idx = inverse_order(fixture.nodes.len(), &node_order);
    let way_order = way_order(fixture);
    let final_way_idx = inverse_order(fixture.ways.len(), &way_order);
    let relation_order = relation_order(fixture);
    let final_relation_idx = inverse_order(fixture.relations.len(), &relation_order);

    let mut tags = TagContext::default();
    let mut tag_index = Vec::new();
    let mut nodes_index = Vec::new();

    {
        let mut nodes = builder.start_nodes()?;
        for &orig in &node_order {
            let first_tag = tags.append(&fixture.nodes[orig].tags, &mut tag_index);
            let node = nodes.grow()?;
            node.set_lon(scale(fixture.nodes[orig].lon));
            node.set_lat(scale(fixture.nodes[orig].lat));
            node.set_tag_first_idx(first_tag);
        }
        nodes.grow()?.set_tag_first_idx(tag_index.len() as u64);
        nodes.close()?;
    }

    {
        let mut ways = builder.start_ways()?;
        for &orig in &way_order {
            let first_tag = tags.append(&fixture.ways[orig].tags, &mut tag_index);
            let way = ways.grow()?;
            way.set_tag_first_idx(first_tag);
            way.set_ref_first_idx(nodes_index.len() as u64);
            for &node_orig in &fixture.ways[orig].refs {
                let mut idx = NodeIndex::new();
                idx.set_value(Some(final_node_idx[node_orig]));
                nodes_index.push(idx);
            }
        }
        let sentinel = ways.grow()?;
        sentinel.set_tag_first_idx(tag_index.len() as u64);
        sentinel.set_ref_first_idx(nodes_index.len() as u64);
        ways.close()?;
    }

    {
        let mut relations = builder.start_relations()?;
        for &orig in &relation_order {
            let first_tag = tags.append(&fixture.relations[orig].tags, &mut tag_index);
            let rel = relations.grow()?;
            rel.set_tag_first_idx(first_tag);
            match fixture.relations[orig].bbox {
                Some((min_lon, min_lat, max_lon, max_lat)) => {
                    rel.set_min_lon(scale(min_lon));
                    rel.set_min_lat(scale(min_lat));
                    rel.set_max_lon(scale(max_lon));
                    rel.set_max_lat(scale(max_lat));
                }
                None => {
                    rel.set_min_lon(RELATION_NO_BBOX[0]);
                    rel.set_min_lat(RELATION_NO_BBOX[1]);
                    rel.set_max_lon(RELATION_NO_BBOX[2]);
                    rel.set_max_lat(RELATION_NO_BBOX[3]);
                }
            }
        }
        relations.grow()?.set_tag_first_idx(tag_index.len() as u64);
        relations.close()?;
    }

    {
        let mut members = builder.start_relation_members()?;
        for &orig in &relation_order {
            let mut bucket = members.grow()?;
            for member in &fixture.relations[orig].members {
                match *member {
                    MemberSpec::Node(node_orig) => {
                        let member = bucket.add_node_member();
                        member.set_node_idx(Some(final_node_idx[node_orig]));
                        member.set_role_idx(0);
                    }
                    MemberSpec::Way(way_orig) => {
                        let member = bucket.add_way_member();
                        member.set_way_idx(Some(final_way_idx[way_orig]));
                        member.set_role_idx(0);
                    }
                    MemberSpec::Relation(relation_orig) => {
                        let member = bucket.add_relation_member();
                        member.set_relation_idx(Some(final_relation_idx[relation_orig]));
                        member.set_role_idx(0);
                    }
                }
            }
        }
        members.close()?;
    }

    builder.set_tags(&tags.tags)?;
    builder.set_tags_index(&tag_index)?;
    builder.set_nodes_index(&nodes_index)?;
    tags.ensure_stringtable();
    builder.set_stringtable(&tags.stringtable)?;

    Ok(Osm::open(storage)?)
}

fn node_order(fixture: &Fixture) -> Vec<usize> {
    let curve = node_curve();
    let mut order: Vec<usize> = (0..fixture.nodes.len()).collect();
    order.sort_by_key(|&i| node_index(&curve, fixture.nodes[i].lon, fixture.nodes[i].lat));
    order
}

fn way_order(fixture: &Fixture) -> Vec<usize> {
    let curve = way_curve();
    let mut order: Vec<usize> = (0..fixture.ways.len()).collect();
    order.sort_by_key(|&i| match way_bbox(fixture, &fixture.ways[i]) {
        Some((min_lon, min_lat, max_lon, max_lat)) => {
            bbox_index(&curve, min_lon, min_lat, max_lon, max_lat)
        }
        None => 0,
    });
    order
}

fn relation_order(fixture: &Fixture) -> Vec<usize> {
    let curve = way_curve();
    let mut order: Vec<usize> = (0..fixture.relations.len()).collect();
    order.sort_by_key(|&i| match fixture.relations[i].bbox {
        Some((min_lon, min_lat, max_lon, max_lat)) => {
            bbox_index(&curve, min_lon, min_lat, max_lon, max_lat)
        }
        None => u64::MAX,
    });
    order
}

fn inverse_order(len: usize, order: &[usize]) -> Vec<u64> {
    let mut inverse = vec![0; len];
    for (final_idx, &orig) in order.iter().enumerate() {
        inverse[orig] = final_idx as u64;
    }
    inverse
}

fn way_bbox(fixture: &Fixture, way: &WaySpec) -> Option<(f64, f64, f64, f64)> {
    let mut bbox = None;
    for &node_idx in &way.refs {
        let node = &fixture.nodes[node_idx];
        bbox = Some(match bbox {
            Some((min_lon, min_lat, max_lon, max_lat)) => (
                f64::min(min_lon, node.lon),
                f64::min(min_lat, node.lat),
                f64::max(max_lon, node.lon),
                f64::max(max_lat, node.lat),
            ),
            None => (node.lon, node.lat, node.lon, node.lat),
        });
    }
    bbox
}

#[derive(Default)]
struct TagContext {
    stringtable: Vec<u8>,
    strings: HashMap<&'static str, u64>,
    slots: HashMap<(&'static str, &'static str), u64>,
    tags: Vec<Tag>,
}

impl TagContext {
    fn ensure_stringtable(&mut self) {
        if self.stringtable.is_empty() {
            self.stringtable.push(0);
            self.strings.insert("", 0);
        }
    }

    fn append(&mut self, entity_tags: &[TagSpec], tag_index: &mut Vec<TagIndex>) -> u64 {
        let first = tag_index.len() as u64;
        for tag in entity_tags {
            let mut entry = TagIndex::new();
            entry.set_value(self.slot(tag));
            tag_index.push(entry);
        }
        first
    }

    fn slot(&mut self, spec: &TagSpec) -> u64 {
        if let Some(&slot) = self.slots.get(&(spec.key, spec.value)) {
            return slot;
        }

        let key_idx = self.intern(spec.key);
        let value_idx = self.intern(spec.value);
        let slot = self.tags.len() as u64;
        let mut tag = Tag::new();
        tag.set_key_idx(key_idx);
        tag.set_value_idx(value_idx);
        self.tags.push(tag);
        self.slots.insert((spec.key, spec.value), slot);
        slot
    }

    fn intern(&mut self, value: &'static str) -> u64 {
        self.ensure_stringtable();
        if let Some(&idx) = self.strings.get(value) {
            return idx;
        }
        let idx = self.stringtable.len() as u64;
        self.stringtable.extend_from_slice(value.as_bytes());
        self.stringtable.push(0);
        self.strings.insert(value, idx);
        idx
    }
}
