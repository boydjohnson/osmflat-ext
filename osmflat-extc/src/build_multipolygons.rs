//! Build the `Multipolygons` sub-archive: precomputed relation ring assembly.
//!
//! Unlike `build_backrefs`/`build_taginfo`, this isn't a scattered-children
//! count-then-fill CSR -- each relation's assembled polygons are known
//! immediately when that relation is visited (via
//! `osmflat_ext::multipolygon::assemble_multipolygon`, the same stitching
//! `osmflat-mapnik-plugin`'s live path uses), and relations are visited in
//! ascending index order already. So this is a single forward pass: for each
//! relation, in order, grow the three nested `ExternalVector`s directly, with
//! a trailing sentinel closing each after the loop -- no scratch/offset
//! counting arrays needed.

use crate::{BuildError, BuildOptions};
use osmflat::Osm;
use osmflat_ext::multipolygon::{assemble_multipolygon, is_area_relation};
use osmflat_ext::MultipolygonsBuilder;

/// Build and write the `Multipolygons` sub-archive for `parent` into `builder`.
pub fn build(
    parent: &Osm,
    builder: &MultipolygonsBuilder,
    _opts: &BuildOptions,
) -> Result<(), BuildError> {
    let mut rel_polygon_range = builder.start_rel_polygon_range()?;
    let mut polygon_ring_range = builder.start_polygon_ring_range()?;
    let mut ring_node_range = builder.start_ring_node_range()?;
    let mut nodes = builder.start_nodes()?;

    // coord_scale is a per-archive constant used to de-scale stored lon/lat
    // back to degrees; assemble_multipolygon needs it to compute distances
    // for ring-closing, even though we only keep node indices from the result.
    let scale = parent.header().coord_scale() as f64;

    let mut polygon_count: u64 = 0;
    let mut ring_count: u64 = 0;
    let mut node_count: u64 = 0;

    for (idx, relation) in parent.relations().iter().enumerate() {
        rel_polygon_range.grow()?.set_first_idx(polygon_count);

        if !is_area_relation(parent, &relation) {
            continue;
        }
        let (polygons, _open_outer_chains) = assemble_multipolygon(parent, idx, scale);

        for polygon in polygons {
            polygon_ring_range.grow()?.set_first_idx(ring_count);
            polygon_count += 1;

            for ring in polygon {
                ring_node_range.grow()?.set_first_idx(node_count);
                ring_count += 1;

                for vertex in ring {
                    nodes.grow()?.set_value(vertex.node_idx);
                    node_count += 1;
                }
            }
        }
    }

    // Trailing sentinels: flatdata's @range reads element i's Range as
    // [ranges[i].first_idx, ranges[i+1].first_idx), so every CSR level needs
    // one more entry than there are real groups to close the last one.
    rel_polygon_range.grow()?.set_first_idx(polygon_count);
    polygon_ring_range.grow()?.set_first_idx(ring_count);
    ring_node_range.grow()?.set_first_idx(node_count);

    rel_polygon_range.close()?;
    polygon_ring_range.close()?;
    ring_node_range.close()?;
    nodes.close()?;
    Ok(())
}
