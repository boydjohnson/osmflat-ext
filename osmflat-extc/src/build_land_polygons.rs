//! Build the `LandPolygons` sub-archive: land polygons imported from an
//! external, already-closed coastline dataset (e.g. osmdata.openstreetmap.de's
//! `land-polygons`), rather than derived from the parent archive's own
//! (open, unclosed) coastline ways.
//!
//! The simplified/split shapefiles from that source are published in Web
//! Mercator (EPSG:3857); every ring is reprojected to WGS84 here so it lines
//! up with the parent archive's own lon/lat coordinates, then sorted by area
//! descending, matching `Coastline`'s painter's-algorithm convention.
//!
//! Land/hole is taken from the shapefile's own `Outer`/`Inner` ring role, not
//! re-derived from signed area: the `shapefile` crate canonicalizes every
//! ring's winding to the ESRI convention (outer rings clockwise, holes
//! counter-clockwise) regardless of how the source data ordered its points,
//! which is the *opposite* sense of the `natural=coastline` "land on the
//! left" convention `Coastline`'s own signed-area check relies on -- using
//! signed area here would classify every landmass as a hole and vice versa.

use crate::{BuildError, BuildOptions};
use osmflat::Osm;
use osmflat_ext::coastline::signed_area_m2;
use osmflat_ext::LandPolygonsBuilder;
use shapefile::PolygonRing;
use std::path::Path;

/// Earth radius (m) used by the Web Mercator (EPSG:3857) projection.
const WEB_MERCATOR_R: f64 = 6378137.0;

fn web_mercator_to_wgs84(x: f64, y: f64) -> (f64, f64) {
    let lon = x / WEB_MERCATOR_R * 180.0 / std::f64::consts::PI;
    let lat = (2.0 * (y / WEB_MERCATOR_R).exp().atan() - std::f64::consts::FRAC_PI_2) * 180.0
        / std::f64::consts::PI;
    (lon, lat)
}

struct Ring {
    is_land: bool,
    area_m2: f64,
    vertices: Vec<(f64, f64)>,
}

/// Whether `vertices`' own bbox overlaps `bbox` (min_lon, min_lat, max_lon, max_lat).
fn bbox_overlaps(vertices: &[(f64, f64)], bbox: (f64, f64, f64, f64)) -> bool {
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    let mut v = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for &(lon, lat) in vertices {
        v.0 = v.0.min(lon);
        v.1 = v.1.min(lat);
        v.2 = v.2.max(lon);
        v.3 = v.3.max(lat);
    }
    v.0 <= max_lon && v.2 >= min_lon && v.1 <= max_lat && v.3 >= min_lat
}

/// Build and write the `LandPolygons` sub-archive for `parent` into `builder`,
/// importing rings from the shapefile at `shapefile_path` (Web Mercator).
pub fn build(
    parent: &Osm,
    builder: &LandPolygonsBuilder,
    shapefile_path: &Path,
    _opts: &BuildOptions,
) -> Result<(), BuildError> {
    let scale = parent.header().coord_scale() as f64;
    let header = parent.header();
    let bbox = (
        header.bbox_left() as f64 / scale,
        header.bbox_bottom() as f64 / scale,
        header.bbox_right() as f64 / scale,
        header.bbox_top() as f64 / scale,
    );

    let shapes = shapefile::read_shapes(shapefile_path).map_err(|e| {
        BuildError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))
    })?;

    let mut rings: Vec<Ring> = Vec::new();
    for shape in shapes {
        let polygon = match shape {
            shapefile::Shape::Polygon(p) => p,
            _ => continue,
        };
        for ring in polygon.rings() {
            let is_land = matches!(ring, PolygonRing::Outer(_));
            let vertices: Vec<(f64, f64)> = ring
                .points()
                .iter()
                .map(|p| web_mercator_to_wgs84(p.x, p.y))
                .collect();
            if vertices.len() < 3 || !bbox_overlaps(&vertices, bbox) {
                continue;
            }
            let area_m2 = signed_area_m2(&vertices).abs();
            rings.push(Ring {
                is_land,
                area_m2,
                vertices,
            });
        }
    }

    rings.sort_by(|a, b| b.area_m2.partial_cmp(&a.area_m2).unwrap());

    let mut ring_entries = builder.start_rings()?;
    let mut coords = builder.start_coords()?;

    let mut coord_count: u64 = 0;
    for ring in &rings {
        let entry = ring_entries.grow()?;
        entry.set_is_land(ring.is_land as u8);
        entry.set_coord_first_idx(coord_count);

        for &(lon, lat) in &ring.vertices {
            let coord = coords.grow()?;
            coord.set_lon((lon * scale).round() as i32);
            coord.set_lat((lat * scale).round() as i32);
            coord_count += 1;
        }
    }
    // Trailing sentinel closes the last ring's `coords()` range.
    ring_entries.grow()?.set_coord_first_idx(coord_count);

    ring_entries.close()?;
    coords.close()?;
    Ok(())
}
