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

    /// Also build taginfo key co-occurrence (phase 2; implies --taginfo).
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
        combinations: args.combinations,
        mmap_scratch: args.mmap_scratch,
    };

    if !opts.taginfo && !opts.backrefs {
        eprintln!("nothing to build: pass --taginfo and/or --backrefs");
        std::process::exit(2);
    }

    osmflat_extc::build(&args.parent, &out, &opts)?;
    Ok(())
}
