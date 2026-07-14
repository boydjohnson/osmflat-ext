//! Build the `Coastline` sub-archive: precomputed global coastline ring
//! assembly, classified land/water and sorted by area.
//!
//! Simpler than `build_multipolygons`: `osmflat_ext::coastline::assemble_coastline`
//! already does the whole assemble-classify-sort job in one call, so this is
//! just a single forward pass writing its result out as two `ExternalVector`s
//! (one entry per ring carrying its own node-range start, one flat node-index
//! vector), plus a trailing sentinel to close the last ring's range.

use crate::{BuildError, BuildOptions};
use osmflat::Osm;
use osmflat_ext::coastline::assemble_coastline;
use osmflat_ext::CoastlineBuilder;

/// Build and write the `Coastline` sub-archive for `parent` into `builder`.
pub fn build(
    parent: &Osm,
    builder: &CoastlineBuilder,
    _opts: &BuildOptions,
) -> Result<(), BuildError> {
    let scale = parent.header().coord_scale() as f64;
    let rings = assemble_coastline(parent, scale);

    let mut ring_entries = builder.start_rings()?;
    let mut nodes = builder.start_nodes()?;

    let mut node_count: u64 = 0;
    for ring in &rings {
        let entry = ring_entries.grow()?;
        entry.set_is_land(ring.is_land as u8);
        entry.set_node_first_idx(node_count);

        for vertex in &ring.vertices {
            nodes.grow()?.set_value(vertex.node_idx);
            node_count += 1;
        }
    }
    // Trailing sentinel closes the last ring's `nodes()` range.
    ring_entries.grow()?.set_node_first_idx(node_count);

    ring_entries.close()?;
    nodes.close()?;
    Ok(())
}
