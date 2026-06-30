//! Build the `Taginfo` sub-archive: inverted tag index + histograms.
//!
//! Three phases (`osmflat-ext-design.md` §6.2), all over the mmapped parent:
//!
//! * **Phase 0 — dictionary.** Scan `tags_index → tags → (key_idx, value_idx)`
//!   to collect the distinct `(key,value)` set and group values under keys.
//!   Sort keys by *string* (resolved against the parent stringtable); within a
//!   key, sort values by string. This fixes each `(k,v)`'s dense slot and emits
//!   the `keys` / `values` vectors (counts filled in phase 2).
//! * **Phase 1 — count.** Per type, iterate entities in index order; for each
//!   tag occurrence bump a per-(slot,type) counter. Prefix-sum into the postings
//!   offsets (`@range` first_idx fields + sentinel).
//! * **Phase 2 — fill.** Iterate entities in index order again; append each
//!   entity index into its `(k,v)` postings slot. In-order iteration makes each
//!   postings run ascending == spatial order, no sort. Accumulate `KeyEntry`
//!   per-type counts here.
//!
//! At planet scale, postings spill to mmap scratch; the `(key,value) → slot`
//! map stays resident (a few hundred MB), or is replaced by an external
//! emit→sort→reduce of `(slot, type, entity)` tuples — see design §6.2.

use crate::{BuildError, BuildOptions};
use osmflat::Osm;
use osmflat_ext::TaginfoBuilder;

/// Build and write the `Taginfo` sub-archive for `parent` into `builder`.
pub fn build(
    _parent: &Osm,
    _builder: &TaginfoBuilder,
    _opts: &BuildOptions,
) -> Result<(), BuildError> {
    // Phase 0
    let _dict = build_dictionary(_parent)?;
    // Phases 1-2
    todo!("count postings, prefix-sum offsets, fill postings; write keys/values/*_post")
}

/// The sorted key/value dictionary and the `(key_idx, value_idx) -> slot` map.
pub struct Dictionary {
    // keys sorted by string; values grouped by key, sorted by string;
    // slot id per distinct (k,v); resident map for phases 1-2.
}

/// Phase 0: collect distinct tags, group by key, sort keys and values by string.
fn build_dictionary(_parent: &Osm) -> Result<Dictionary, BuildError> {
    todo!("scan tags_index/tags; group values under keys; sort by parent stringtable strings")
}
