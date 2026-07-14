//! Query support for the `LandPolygons` sub-archive: land polygons imported
//! from an external, already-closed coastline dataset (see
//! `osmflat-extc --land-polygons`), not derived from the parent archive at
//! all. Read-only -- the import/build side lives in `osmflat-extc`.

/// One land (or hole) polygon ring, already resolved to `(lon, lat)`
/// vertices -- there's no parent archive to resolve indices against, unlike
/// every other resource in this crate.
pub struct LandPolygonRingView {
    pub is_land: bool,
    pub vertices: Vec<(f64, f64)>,
}

/// Query API over a precomputed `LandPolygons` sub-archive.
#[derive(Clone, Copy)]
pub struct LandPolygonsQuery<'a> {
    land_polygons: &'a crate::LandPolygons,
    coord_scale: f64,
}

impl<'a> LandPolygonsQuery<'a> {
    /// Wrap a `LandPolygons` sub-archive. `coord_scale` must be the *parent*
    /// archive's `header.coord_scale` -- the same one the sidecar was built
    /// with (see `osmflat-extc --land-polygons`). Prefer
    /// [`crate::ExtArchive::land_polygons`], which resolves this from the
    /// verified parent automatically.
    #[inline]
    pub fn new(land_polygons: &'a crate::LandPolygons, coord_scale: f64) -> Self {
        Self {
            land_polygons,
            coord_scale,
        }
    }

    /// All rings, in stored order (enclosed area descending -- see the
    /// schema docs for why that order matters for rendering).
    pub fn rings(&self) -> Vec<LandPolygonRingView> {
        let coords = self.land_polygons.coords();
        self.land_polygons
            .rings()
            .iter()
            .map(|entry| {
                let r = entry.coords();
                let vertices = coords[r.start as usize..r.end as usize]
                    .iter()
                    .map(|c| {
                        (
                            c.lon() as f64 / self.coord_scale,
                            c.lat() as f64 / self.coord_scale,
                        )
                    })
                    .collect();
                LandPolygonRingView {
                    is_land: entry.is_land() != 0,
                    vertices,
                }
            })
            .collect()
    }
}
