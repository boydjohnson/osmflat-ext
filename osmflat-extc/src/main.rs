//! `osmflat-extc` — build sidecar index archives from an osmflat archive.

use clap::Parser;
use std::path::PathBuf;

/// Known limitations, shown by `--help` (design doc §10).
const LIMITATIONS: &str = "\
Known limitations:
  1. A sidecar is bound to one parent build. It stores parent indices, so it is
     invalid against any other or rebuilt parent; opening it against one fails
     with a fingerprint mismatch. Rebuild the sidecar whenever the parent changes.
  2. Build cost scales with the data. Postings are proportional to the parent's
     tag occurrences; at planet scale use --mmap-scratch on a fast disk.
  3. --combinations is only partly backed by --mmap-scratch: raw tag pairs go to
     scratch, but key-pair counts, each bucket while it is sorted, and the final
     per-tag co-occurrence lists are held in RAM. At planet scale use a machine
     with RAM to match, or skip the flag.
  4. Only exact and key-prefix lookups are indexed; there is no substring search.
  5. No geometry is stored (except --land-polygons, which stores the imported
     coordinates). Spatial queries recompute from the parent.
  6. --coastline only produces closed rings: islands and fully enclosed water.
     A mainland coastline in a bounded extract never closes; use
     --land-polygons <shapefile> for mainland land fill.";

/// Build osmflat-ext sidecar archives (inverted tag index / taginfo, reverse
/// references, precomputed multipolygon / coastline / land-polygon rings)
/// from an existing osmflat archive.
#[derive(Parser, Debug)]
#[command(
    name = "osmflat-extc",
    version,
    after_help = "See --help for known limitations.",
    after_long_help = LIMITATIONS
)]
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
