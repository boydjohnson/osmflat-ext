//! `osmflat-extc` — build sidecar index archives from an osmflat archive.

use clap::Parser;
use std::path::PathBuf;

/// Build osmflat-ext sidecar archives (inverted tag index / taginfo, reverse
/// references) from an existing osmflat archive.
#[derive(Parser, Debug)]
#[command(name = "osmflat-extc", version)]
struct Args {
    /// Input osmflat archive directory (the parent `Osm` archive).
    parent: PathBuf,

    /// Output Ext archive directory. Defaults to a sibling `<parent>.ext`.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Build the Taginfo sub-archive (inverted tag index + histograms).
    #[arg(long)]
    taginfo: bool,

    /// Build the Backrefs sub-archive (node->ways, X->relations).
    #[arg(long)]
    backrefs: bool,

    /// Build the Multipolygons sub-archive (precomputed relation ring
    /// assembly, so renderers don't re-stitch outer/inner ways on every
    /// query).
    #[arg(long)]
    multipolygons: bool,

    /// Build the Coastline sub-archive (precomputed global coastline ring
    /// assembly, classified land/water -- mirrors `osmcoastline`).
    #[arg(long)]
    coastline: bool,

    /// Build the LandPolygons sub-archive by importing rings from an
    /// external, already-closed coastline shapefile (Web Mercator/EPSG:3857),
    /// e.g. osmdata.openstreetmap.de's `land-polygons` dataset.
    #[arg(long)]
    land_polygons: Option<PathBuf>,

    /// Also build taginfo key and tag co-occurrence (implies --taginfo).
    #[arg(long)]
    combinations: bool,

    /// Back the postings build with mmap temp files here (planet scale).
    #[arg(long)]
    mmap_scratch: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let out = args.out.unwrap_or_else(|| {
        let mut p = args.parent.clone().into_os_string();
        p.push(".ext");
        PathBuf::from(p)
    });

    let opts = osmflat_extc::BuildOptions {
        taginfo: args.taginfo || args.combinations,
        backrefs: args.backrefs,
        multipolygons: args.multipolygons,
        coastline: args.coastline,
        land_polygons: args.land_polygons,
        combinations: args.combinations,
        mmap_scratch: args.mmap_scratch,
    };

    if !opts.taginfo
        && !opts.backrefs
        && !opts.multipolygons
        && !opts.coastline
        && opts.land_polygons.is_none()
    {
        eprintln!(
            "nothing to build: pass --taginfo, --backrefs, --multipolygons, --coastline, and/or --land-polygons <path>"
        );
        std::process::exit(2);
    }

    osmflat_extc::build(&args.parent, &out, &opts)?;
    Ok(())
}
