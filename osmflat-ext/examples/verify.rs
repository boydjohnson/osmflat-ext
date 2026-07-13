//! Verify a built `Ext` sidecar is laid out correctly against its parent.
//!
//! Checks, in order:
//! * the fingerprint (parent schema hash + vector lengths) — via `ExtArchive::open`
//! * every `@range` field is monotonic and its runs exactly cover the backing
//!   array (no gaps, no overlap, no leftover tail)
//! * every stored index (postings, combo key/value refs) is within bounds of
//!   the array it indexes into
//! * documented sort orders hold: keys/values by string, combinations by
//!   (count desc, string...)
//! * aggregate counts stored in `KeyEntry` equal the sum of its values'
//!   postings lengths, and total postings lengths equal an independently
//!   computed sum over the parent
//!
//! A full O(N) cross-reference of "does this entity really carry this tag /
//! really reference this member" would cost as much as the original build,
//! so that's instead checked on a deterministic stride sample (`--sample`
//! controls roughly how many total samples, spread evenly — no RNG, so runs
//! are reproducible).
//!
//! ```text
//! cargo run --release --example verify -- planet.osm.flatdata planet.osm.ext
//! cargo run --release --example verify -- planet.osm.flatdata planet.osm.ext --sample 200000
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use clap::Parser;
use osmflat::{FileResourceStorage, Osm, RelationMembersRef};
use osmflat_ext::{Backrefs, Ext, ExtArchive, Range, Ref, Taginfo};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(about = "Verify an osmflat-ext sidecar's layout against its parent")]
struct Args {
    /// Parent osmflat archive directory.
    parent: PathBuf,
    /// Sibling Ext archive directory.
    ext: PathBuf,
    /// Approximate total number of ground-truth cross-check samples per
    /// resource, spread evenly by stride (not random — deterministic).
    #[arg(long, default_value_t = 20_000)]
    sample: usize,
}

/// One named check, accumulating a failure count and a handful of examples
/// so a badly-broken archive still prints readable output.
struct Check {
    name: &'static str,
    failures: u64,
    examples: Vec<String>,
}

impl Check {
    fn new(name: &'static str) -> Self {
        Check {
            name,
            failures: 0,
            examples: Vec::new(),
        }
    }

    fn fail(&mut self, detail: impl Into<String>) {
        self.failures += 1;
        if self.examples.len() < 5 {
            self.examples.push(detail.into());
        }
    }

    fn report(&self) {
        if self.failures == 0 {
            println!("  ok    {}", self.name);
        } else {
            println!("  FAIL  {} — {} failure(s)", self.name, self.failures);
            for e in &self.examples {
                println!("          {e}");
            }
            let more = self.failures as usize - self.examples.len();
            if more > 0 {
                println!("          … (+{more} more)");
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let start = Instant::now();

    let parent = Osm::open(FileResourceStorage::new(args.parent))?;
    let ext = Ext::open(FileResourceStorage::new(args.ext))?;
    let archive = ExtArchive::open(parent, ext)?;
    println!("fingerprint: ok (parent schema + vector lengths match)\n");
    let parent = archive.parent();

    let mut checks: Vec<Check> = Vec::new();

    if let Some(taginfo) = archive.ext().taginfo() {
        println!("=== taginfo ===");
        checks.extend(verify_taginfo(parent, taginfo, args.sample));
    } else {
        println!("(no taginfo sub-archive — skipping)");
    }

    if let Some(backrefs) = archive.ext().backrefs() {
        println!("\n=== backrefs ===");
        checks.extend(verify_backrefs(parent, backrefs, args.sample));
    } else {
        println!("(no backrefs sub-archive — skipping)");
    }

    println!();
    let mut any_fail = false;
    for c in &checks {
        c.report();
        any_fail |= c.failures > 0;
    }
    println!("\nchecked in {:?}", start.elapsed());

    if any_fail {
        eprintln!("\nVERIFICATION FAILED");
        std::process::exit(1);
    }
    println!("\nVERIFICATION OK");
    Ok(())
}

enum EntityKind {
    Node,
    Way,
    Relation,
}

/// Does the entity at `idx` actually carry a tag with this exact
/// `(key_idx, value_idx)`? The ground-truth check for postings.
fn entity_has_tag(
    parent: &Osm,
    kind: &EntityKind,
    idx: usize,
    key_idx: u64,
    value_idx: u64,
) -> bool {
    let tags_index = parent.tags_index();
    let tags = parent.tags();
    let range = match kind {
        EntityKind::Node => parent.nodes()[idx].tags(),
        EntityKind::Way => parent.ways()[idx].tags(),
        EntityKind::Relation => parent.relations()[idx].tags(),
    };
    range.into_iter().any(|ti| {
        let slot = tags_index[ti as usize].value() as usize;
        let tag = &tags[slot];
        tag.key_idx() == key_idx && tag.value_idx() == value_idx
    })
}

/// Checks one postings run (`posts[range]`) against the parent: bounds,
/// ascending order, contiguity with the previous run (via `next`), and a
/// strided ground-truth sample. Returns the run's length.
#[allow(clippy::too_many_arguments)]
fn check_postings_run(
    posts: &[Ref],
    range: std::ops::Range<u64>,
    n_targets: u64,
    kind: EntityKind,
    key_idx: u64,
    value_idx: u64,
    parent: &Osm,
    label: &str,
    next: &mut u64,
    pos: &mut u64,
    stride: u64,
    c_contig: &mut Check,
    c_bounds: &mut Check,
    c_ascending: &mut Check,
    c_ground_truth: &mut Check,
) -> u64 {
    if range.start != *next {
        c_contig.fail(format!(
            "{label}: run starts at {} but expected {}",
            range.start, *next
        ));
    }
    if range.end < range.start || range.end as usize > posts.len() {
        c_contig.fail(format!(
            "{label}: invalid range {}..{} (array len {})",
            range.start,
            range.end,
            posts.len()
        ));
        return 0;
    }
    *next = range.end;

    let mut prev: Option<u64> = None;
    for r in &posts[range.start as usize..range.end as usize] {
        let v = r.value();
        if v >= n_targets {
            c_bounds.fail(format!("{label}: index {v} >= {n_targets}"));
        }
        if let Some(pv) = prev {
            if v < pv {
                c_ascending.fail(format!("{label}: not ascending ({pv} then {v})"));
            }
        }
        prev = Some(v);

        *pos += 1;
        if *pos % stride == 0 && v < n_targets {
            if !entity_has_tag(parent, &kind, v as usize, key_idx, value_idx) {
                c_ground_truth.fail(format!(
                    "{label}: entity {v} listed but doesn't carry key_idx={key_idx} value_idx={value_idx}"
                ));
            }
        }
    }
    range.end - range.start
}

fn verify_taginfo(parent: &Osm, taginfo: &Taginfo, sample_target: usize) -> Vec<Check> {
    let mut c_keys_sorted = Check::new("keys sorted by key string");
    let mut c_values_sorted = Check::new("values sorted by value string within each key");
    let mut c_key_values_contig = Check::new("KeyEntry.values() ranges contiguous over `values`");
    let mut c_key_combos_contig = Check::new("KeyEntry.combos() ranges contiguous over `combos`");
    let mut c_tagcombos_contig =
        Check::new("ValueEntry.tag_combos() ranges contiguous over `tag_combos`");
    let mut c_node_post_contig =
        Check::new("ValueEntry.node_post() ranges contiguous over `node_post`");
    let mut c_way_post_contig =
        Check::new("ValueEntry.way_post() ranges contiguous over `way_post`");
    let mut c_rel_post_contig =
        Check::new("ValueEntry.rel_post() ranges contiguous over `rel_post`");
    let mut c_bounds = Check::new("postings indices within parent bounds");
    let mut c_ascending = Check::new("postings ascending within each run");
    let mut c_counts_match =
        Check::new("KeyEntry counts equal sum of its values' postings lengths");
    let mut c_combo_order = Check::new("key combinations sorted by (count desc, key string)");
    let mut c_combo_bound = Check::new("key combination together_count <= key's own object count");
    let mut c_tagcombo_order =
        Check::new("tag combinations sorted by (count desc, key string, value string)");
    let mut c_tagcombo_bound =
        Check::new("tag combination together_count <= tag's own object count");
    let mut c_total_posts =
        Check::new("total postings length matches an independent sum over the parent");
    let mut c_ground_truth = Check::new("sampled postings entities actually carry the claimed tag");

    let keys = taginfo.keys();
    let values = taginfo.values();
    let combos = taginfo.combos();
    let tag_combos = taginfo.tag_combos();
    let node_post = taginfo.node_post();
    let way_post = taginfo.way_post();
    let rel_post = taginfo.rel_post();

    let n_nodes = parent.nodes().len() as u64;
    let n_ways = parent.ways().len() as u64;
    let n_rels = parent.relations().len() as u64;
    let strtab = parent.stringtable();
    let s = |idx: u64| strtab.substring_raw(idx as usize);

    let total_posts = node_post.len() as u64 + way_post.len() as u64 + rel_post.len() as u64;
    let stride = (total_posts / sample_target.max(1) as u64).max(1);
    let mut pos = 0u64;

    let (mut next_value, mut next_combo, mut next_tag_combo) = (0u64, 0u64, 0u64);
    let (mut next_node, mut next_way, mut next_rel) = (0u64, 0u64, 0u64);
    let mut prev_key: Option<&[u8]> = None;

    for key in keys {
        let key_str = s(key.key_idx());
        if let Some(p) = prev_key {
            if key_str < p {
                c_keys_sorted.fail(format!(
                    "{:?} came after {:?}",
                    String::from_utf8_lossy(key_str),
                    String::from_utf8_lossy(p)
                ));
            }
        }
        prev_key = Some(key_str);

        let vr = key.values();
        if vr.start != next_value {
            c_key_values_contig.fail(format!(
                "key {:?}: values range starts at {} but expected {}",
                String::from_utf8_lossy(key_str),
                vr.start,
                next_value
            ));
        }
        if vr.end < vr.start || vr.end as usize > values.len() {
            c_key_values_contig.fail(format!(
                "key {:?}: invalid values range {}..{}",
                String::from_utf8_lossy(key_str),
                vr.start,
                vr.end
            ));
            continue;
        }
        next_value = vr.end;

        let cr = key.combos();
        if cr.start != next_combo {
            c_key_combos_contig.fail(format!(
                "key {:?}: combos range starts at {} but expected {}",
                String::from_utf8_lossy(key_str),
                cr.start,
                next_combo
            ));
        }
        if cr.end < cr.start || cr.end as usize > combos.len() {
            c_key_combos_contig.fail(format!(
                "key {:?}: invalid combos range {}..{}",
                String::from_utf8_lossy(key_str),
                cr.start,
                cr.end
            ));
        } else {
            next_combo = cr.end;
            let key_total = key.count_nodes() + key.count_ways() + key.count_relations();
            let mut prev_combo: Option<(u64, &[u8])> = None;
            for combo in &combos[cr.start as usize..cr.end as usize] {
                let other = s(combo.other_key_idx());
                let tc = combo.together_count();
                if tc > key_total {
                    c_combo_bound.fail(format!(
                        "key {:?}: combo with {:?} has together_count {tc} > key total {key_total}",
                        String::from_utf8_lossy(key_str),
                        String::from_utf8_lossy(other)
                    ));
                }
                if let Some((ptc, pother)) = prev_combo {
                    let ok = ptc > tc || (ptc == tc && pother <= other);
                    if !ok {
                        c_combo_order.fail(format!(
                            "key {:?}: combo order violated at {:?} (count {tc}) after {:?} (count {ptc})",
                            String::from_utf8_lossy(key_str),
                            String::from_utf8_lossy(other),
                            String::from_utf8_lossy(pother)
                        ));
                    }
                }
                prev_combo = Some((tc, other));
            }
        }

        let mut prev_value: Option<&[u8]> = None;
        let (mut sum_nodes, mut sum_ways, mut sum_rels) = (0u64, 0u64, 0u64);

        for value in &values[vr.start as usize..vr.end as usize] {
            let value_str = s(value.value_idx());
            if let Some(p) = prev_value {
                if value_str < p {
                    c_values_sorted.fail(format!(
                        "key {:?}: {:?} came after {:?}",
                        String::from_utf8_lossy(key_str),
                        String::from_utf8_lossy(value_str),
                        String::from_utf8_lossy(p)
                    ));
                }
            }
            prev_value = Some(value_str);

            let label_n = format!(
                "node_post for {:?}={:?}",
                String::from_utf8_lossy(key_str),
                String::from_utf8_lossy(value_str)
            );
            sum_nodes += check_postings_run(
                node_post,
                value.node_post(),
                n_nodes,
                EntityKind::Node,
                key.key_idx(),
                value.value_idx(),
                parent,
                &label_n,
                &mut next_node,
                &mut pos,
                stride,
                &mut c_node_post_contig,
                &mut c_bounds,
                &mut c_ascending,
                &mut c_ground_truth,
            );
            let label_w = format!(
                "way_post for {:?}={:?}",
                String::from_utf8_lossy(key_str),
                String::from_utf8_lossy(value_str)
            );
            sum_ways += check_postings_run(
                way_post,
                value.way_post(),
                n_ways,
                EntityKind::Way,
                key.key_idx(),
                value.value_idx(),
                parent,
                &label_w,
                &mut next_way,
                &mut pos,
                stride,
                &mut c_way_post_contig,
                &mut c_bounds,
                &mut c_ascending,
                &mut c_ground_truth,
            );
            let label_r = format!(
                "rel_post for {:?}={:?}",
                String::from_utf8_lossy(key_str),
                String::from_utf8_lossy(value_str)
            );
            sum_rels += check_postings_run(
                rel_post,
                value.rel_post(),
                n_rels,
                EntityKind::Relation,
                key.key_idx(),
                value.value_idx(),
                parent,
                &label_r,
                &mut next_rel,
                &mut pos,
                stride,
                &mut c_rel_post_contig,
                &mut c_bounds,
                &mut c_ascending,
                &mut c_ground_truth,
            );

            let tcr = value.tag_combos();
            if tcr.start != next_tag_combo {
                c_tagcombos_contig.fail(format!(
                    "{:?}={:?}: tag_combos range starts at {} but expected {}",
                    String::from_utf8_lossy(key_str),
                    String::from_utf8_lossy(value_str),
                    tcr.start,
                    next_tag_combo
                ));
            }
            if tcr.end < tcr.start || tcr.end as usize > tag_combos.len() {
                c_tagcombos_contig.fail(format!(
                    "{:?}={:?}: invalid tag_combos range {}..{}",
                    String::from_utf8_lossy(key_str),
                    String::from_utf8_lossy(value_str),
                    tcr.start,
                    tcr.end
                ));
            } else {
                next_tag_combo = tcr.end;
                let value_total = (value.node_post().end - value.node_post().start)
                    + (value.way_post().end - value.way_post().start)
                    + (value.rel_post().end - value.rel_post().start);
                let mut prev_tc: Option<(u64, &[u8], &[u8])> = None;
                for tc_entry in &tag_combos[tcr.start as usize..tcr.end as usize] {
                    let other_key = s(tc_entry.other_key_idx());
                    let other_value = s(tc_entry.other_value_idx());
                    let tc = tc_entry.together_count();
                    if tc > value_total {
                        c_tagcombo_bound.fail(format!(
                            "{:?}={:?}: combo with {:?}={:?} has together_count {tc} > tag total {value_total}",
                            String::from_utf8_lossy(key_str),
                            String::from_utf8_lossy(value_str),
                            String::from_utf8_lossy(other_key),
                            String::from_utf8_lossy(other_value),
                        ));
                    }
                    if let Some((ptc, pk, pv)) = prev_tc {
                        let ok = ptc > tc
                            || (ptc == tc && pk < other_key)
                            || (ptc == tc && pk == other_key && pv <= other_value);
                        if !ok {
                            c_tagcombo_order.fail(format!(
                                "{:?}={:?}: tag combo order violated at {:?}={:?} (count {tc})",
                                String::from_utf8_lossy(key_str),
                                String::from_utf8_lossy(value_str),
                                String::from_utf8_lossy(other_key),
                                String::from_utf8_lossy(other_value),
                            ));
                        }
                    }
                    prev_tc = Some((tc, other_key, other_value));
                }
            }
        }

        if sum_nodes != key.count_nodes()
            || sum_ways != key.count_ways()
            || sum_rels != key.count_relations()
        {
            c_counts_match.fail(format!(
                "key {:?}: stored counts (n={}, w={}, r={}) != sum of values' postings (n={sum_nodes}, w={sum_ways}, r={sum_rels})",
                String::from_utf8_lossy(key_str),
                key.count_nodes(),
                key.count_ways(),
                key.count_relations(),
            ));
        }
    }

    if next_value != values.len() as u64 {
        c_key_values_contig.fail(format!(
            "coverage ends at {next_value}, `values` len {}",
            values.len()
        ));
    }
    if next_node != node_post.len() as u64 {
        c_node_post_contig.fail(format!(
            "coverage ends at {next_node}, `node_post` len {}",
            node_post.len()
        ));
    }
    if next_way != way_post.len() as u64 {
        c_way_post_contig.fail(format!(
            "coverage ends at {next_way}, `way_post` len {}",
            way_post.len()
        ));
    }
    if next_rel != rel_post.len() as u64 {
        c_rel_post_contig.fail(format!(
            "coverage ends at {next_rel}, `rel_post` len {}",
            rel_post.len()
        ));
    }

    // Independent ground truth for total postings volume: every node-tag
    // reference in the parent contributes exactly one node_post entry
    // (one per (entity, tag) pair), regardless of which (key,value) it's
    // grouped under.
    let sum_node_tags: u64 = parent
        .nodes()
        .iter()
        .map(|n| n.tags().end - n.tags().start)
        .sum();
    let sum_way_tags: u64 = parent
        .ways()
        .iter()
        .map(|w| w.tags().end - w.tags().start)
        .sum();
    let sum_rel_tags: u64 = parent
        .relations()
        .iter()
        .map(|r| r.tags().end - r.tags().start)
        .sum();
    if sum_node_tags != node_post.len() as u64 {
        c_total_posts.fail(format!(
            "node_post has {} entries, parent nodes carry {sum_node_tags} tag references",
            node_post.len()
        ));
    }
    if sum_way_tags != way_post.len() as u64 {
        c_total_posts.fail(format!(
            "way_post has {} entries, parent ways carry {sum_way_tags} tag references",
            way_post.len()
        ));
    }
    if sum_rel_tags != rel_post.len() as u64 {
        c_total_posts.fail(format!(
            "rel_post has {} entries, parent relations carry {sum_rel_tags} tag references",
            rel_post.len()
        ));
    }

    vec![
        c_keys_sorted,
        c_values_sorted,
        c_key_values_contig,
        c_key_combos_contig,
        c_tagcombos_contig,
        c_node_post_contig,
        c_way_post_contig,
        c_rel_post_contig,
        c_bounds,
        c_ascending,
        c_counts_match,
        c_combo_order,
        c_combo_bound,
        c_tagcombo_order,
        c_tagcombo_bound,
        c_total_posts,
        c_ground_truth,
    ]
}

/// Checks one `(ranges, posts)` CSR pair: `ranges` has one entry per parent
/// entity, each range's postings are contiguous/monotonic across `posts`,
/// strictly ascending within a run (backrefs are deduped by construction),
/// and every posting is within `n_targets`.
fn check_range_array(
    ranges: &[Range],
    posts: &[Ref],
    n_parents: usize,
    n_targets: u64,
    label: &str,
    c_lengths: &mut Check,
    c_contig: &mut Check,
    c_bounds: &mut Check,
    c_ascending: &mut Check,
) {
    if ranges.len() != n_parents {
        c_lengths.fail(format!(
            "{label}: {} range entries, expected {n_parents} (one per parent entity)",
            ranges.len()
        ));
    }
    let mut next = 0u64;
    for (i, r) in ranges.iter().enumerate() {
        let pr = r.post();
        if pr.start != next {
            c_contig.fail(format!(
                "{label}[{i}]: run starts at {} but expected {next}",
                pr.start
            ));
        }
        if pr.end < pr.start || pr.end as usize > posts.len() {
            c_contig.fail(format!(
                "{label}[{i}]: invalid range {}..{} (array len {})",
                pr.start,
                pr.end,
                posts.len()
            ));
            continue;
        }
        next = pr.end;

        let mut prev: Option<u64> = None;
        for p in &posts[pr.start as usize..pr.end as usize] {
            let v = p.value();
            if v >= n_targets {
                c_bounds.fail(format!("{label}[{i}]: index {v} >= {n_targets}"));
            }
            if let Some(pv) = prev {
                if v <= pv {
                    c_ascending.fail(format!(
                        "{label}[{i}]: not strictly ascending ({pv} then {v})"
                    ));
                }
            }
            prev = Some(v);
        }
    }
    if next != posts.len() as u64 {
        c_contig.fail(format!(
            "{label}: coverage ends at {next}, array len {}",
            posts.len()
        ));
    }
}

fn verify_backrefs(parent: &Osm, backrefs: &Backrefs, sample_target: usize) -> Vec<Check> {
    let mut c_lengths = Check::new("range arrays have one entry per parent entity");
    let mut c_contig = Check::new("range postings are contiguous and monotonic");
    let mut c_bounds = Check::new("posting indices within target-type bounds");
    let mut c_ascending = Check::new("postings strictly ascending within each range (deduped)");
    let mut c_ground_truth = Check::new("sampled backrefs actually reference back (ground truth)");

    let n_nodes = parent.nodes().len();
    let n_ways = parent.ways().len();
    let n_rels = parent.relations().len();

    check_range_array(
        backrefs.node_ways_range(),
        backrefs.ways_of_node(),
        n_nodes,
        n_ways as u64,
        "node_ways_range/ways_of_node",
        &mut c_lengths,
        &mut c_contig,
        &mut c_bounds,
        &mut c_ascending,
    );
    check_range_array(
        backrefs.node_rels_range(),
        backrefs.rels_of_node(),
        n_nodes,
        n_rels as u64,
        "node_rels_range/rels_of_node",
        &mut c_lengths,
        &mut c_contig,
        &mut c_bounds,
        &mut c_ascending,
    );
    check_range_array(
        backrefs.way_rels_range(),
        backrefs.rels_of_way(),
        n_ways,
        n_rels as u64,
        "way_rels_range/rels_of_way",
        &mut c_lengths,
        &mut c_contig,
        &mut c_bounds,
        &mut c_ascending,
    );
    check_range_array(
        backrefs.rel_rels_range(),
        backrefs.rels_of_rel(),
        n_rels,
        n_rels as u64,
        "rel_rels_range/rels_of_rel",
        &mut c_lengths,
        &mut c_contig,
        &mut c_bounds,
        &mut c_ascending,
    );

    let nodes_index = parent.nodes_index();
    let members = parent.relation_members();

    let stride_nodes = (n_nodes / sample_target.max(1)).max(1);
    for idx in (0..n_nodes).step_by(stride_nodes) {
        if let Some(r) = backrefs.node_ways_range().get(idx) {
            let pr = r.post();
            if pr.end as usize <= backrefs.ways_of_node().len() {
                for w in &backrefs.ways_of_node()[pr.start as usize..pr.end as usize] {
                    let way_idx = w.value() as usize;
                    if way_idx >= n_ways {
                        continue;
                    }
                    let has_it = parent.ways()[way_idx]
                        .refs()
                        .any(|ri| nodes_index[ri as usize].value() == Some(idx as u64));
                    if !has_it {
                        c_ground_truth.fail(format!(
                            "node {idx}: way {way_idx} listed in ways_of_node but doesn't reference it"
                        ));
                    }
                }
            }
        }
        if let Some(r) = backrefs.node_rels_range().get(idx) {
            let pr = r.post();
            if pr.end as usize <= backrefs.rels_of_node().len() {
                for rr in &backrefs.rels_of_node()[pr.start as usize..pr.end as usize] {
                    let rel_idx = rr.value() as usize;
                    if rel_idx >= n_rels {
                        continue;
                    }
                    let has_it = members.at(rel_idx).any(|m| {
                        matches!(m, RelationMembersRef::NodeMember(mm) if mm.node_idx() == Some(idx as u64))
                    });
                    if !has_it {
                        c_ground_truth.fail(format!(
                            "node {idx}: relation {rel_idx} listed in rels_of_node but doesn't contain it"
                        ));
                    }
                }
            }
        }
    }

    let stride_ways = (n_ways / sample_target.max(1)).max(1);
    for idx in (0..n_ways).step_by(stride_ways) {
        if let Some(r) = backrefs.way_rels_range().get(idx) {
            let pr = r.post();
            if pr.end as usize <= backrefs.rels_of_way().len() {
                for rr in &backrefs.rels_of_way()[pr.start as usize..pr.end as usize] {
                    let rel_idx = rr.value() as usize;
                    if rel_idx >= n_rels {
                        continue;
                    }
                    let has_it = members.at(rel_idx).any(|m| {
                        matches!(m, RelationMembersRef::WayMember(mm) if mm.way_idx() == Some(idx as u64))
                    });
                    if !has_it {
                        c_ground_truth.fail(format!(
                            "way {idx}: relation {rel_idx} listed in rels_of_way but doesn't contain it"
                        ));
                    }
                }
            }
        }
    }

    let stride_rels = (n_rels / sample_target.max(1)).max(1);
    for idx in (0..n_rels).step_by(stride_rels) {
        if let Some(r) = backrefs.rel_rels_range().get(idx) {
            let pr = r.post();
            if pr.end as usize <= backrefs.rels_of_rel().len() {
                for rr in &backrefs.rels_of_rel()[pr.start as usize..pr.end as usize] {
                    let rel_idx = rr.value() as usize;
                    if rel_idx >= n_rels {
                        continue;
                    }
                    let has_it = members.at(rel_idx).any(|m| {
                        matches!(m, RelationMembersRef::RelationMember(mm) if mm.relation_idx() == Some(idx as u64))
                    });
                    if !has_it {
                        c_ground_truth.fail(format!(
                            "relation {idx}: relation {rel_idx} listed in rels_of_rel but doesn't contain it"
                        ));
                    }
                }
            }
        }
    }

    vec![c_lengths, c_contig, c_bounds, c_ascending, c_ground_truth]
}
