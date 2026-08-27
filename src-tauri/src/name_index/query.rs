//! The ranking contract over the Name Index (SPEC §6).
//!
//! This is the whole ranker in one module: the scan, the score, and the deterministic
//! order. §6 splits into *guarantees* (binding, in precedence order) and *weights*
//! (tuning). The guarantees are encoded as score bands wide enough that no combination
//! of tuning weights can cross a band — e.g. an exact match minus the heaviest penalty
//! still outscores any fuzzy match plus every boost, so "exact beats any penalty" holds
//! whatever the constants below become. All weights live in the `weights` block so the
//! tuning surface is one place.
//!
//! Guarantee → code map (each also has a focused test, see the `tests` module and
//! `mod.rs`):
//!   a. existing typed path first  → [`existing_typed_path`] injects at [`EXISTING_PATH`]
//!   b. exact beats any penalty    → [`QUAL_EXACT`] band gap > all penalties + boosts
//!   c. path-shaped scope priority → [`score_path_shaped`] (+[`SCOPE_BONUS`], no hidden penalty)
//!   d. RU/EN layout correction    → [`layout_variants`] (−[`CORRECTION_PENALTY`])
//!   e. Visit Journal boost        → `ctx.journal.boost` (≤ `VISIT_CAP`)
//!   f. Known Places boost         → [`known_places`] (+[`KNOWN_PLACE_BOOST`])
//!   g. Alias Dictionary recommend → `ctx.aliases` injects at [`ALIAS_RECOMMEND`]
//!   h. hidden light / junk heavy  → [`HIDDEN_PENALTY`] / [`JUNK_PENALTY`]
//! Ties break by shorter path, then lexicographic path — so the same index + journal +
//! query always yields the same order.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use memchr::{memchr2_iter, memchr_iter};
use serde::Serialize;

use crate::name_index::{
    alias::AliasDictionary,
    model::{name_filter, DirId, EntryRef, IndexData, Tier},
    visit_journal::Aggregate,
};

/// Upper bound on candidates collected before ranking. Bounding what we *keep* (not what
/// we scan) caps the sort and path-reconstruction cost so search stays within the §10
/// Instant budget (≤50 ms) even when a query matches a large fraction of the index.
const MAX_CANDIDATES: usize = 4096;

/// Below this many index slots a full scan runs on the calling thread; above it the scan is
/// split across cores. The threshold keeps the tiny indexes in tests (and a cold,
/// nearly-empty index) from paying thread-spawn overhead for no gain.
const PAR_THRESHOLD: usize = 50_000;

/// Cold scans are memory-bound once eight workers chase Item-name pointers and build path
/// masks. More workers add transient per-shard caches and contend for memory bandwidth: the
/// reference 10-core machine measures 8 workers consistently faster than 10 on its live 5M
/// index. Smaller machines still use every available core.
const MAX_SEARCH_SHARDS: usize = 8;

/// How many index slots each shard scans between stale-query checks. A few thousand entries
/// is frequent enough to abort a superseded scan promptly (well under a millisecond of extra
/// work at ~tens of millions of entries/s) yet rare enough that the atomic load never shows
/// up in the scan cost.
const ABORT_STRIDE: usize = 4096;

// --- weights (the entire tuning surface, SPEC §6) --------------------------------------
// `Q` is the match-quality unit. Quality bands are multiples of `Q`; every penalty and
// boost is far below `Q`, so match quality always dominates and the guarantee bands never
// cross. Injected bands (existing path, alias) sit far above any scanned score.

/// Match-quality unit; one full band. All penalties and boosts combined stay below it.
const Q: i64 = 1_000_000;
/// Whole-name match (case-insensitive). Beats any penalty (guarantee b).
const QUAL_EXACT: i64 = 5 * Q;
/// The query is a prefix of the name.
const QUAL_PREFIX: i64 = 4 * Q;
/// The query occurs inside the name.
const QUAL_SUBSTRING: i64 = 2 * Q;
/// A token of a multi-token query found only in the entry's *path* (not its name). One full
/// band below the weakest name match, so an all-tokens-in-name hit always outranks a hit
/// where any token only appears in the path (SPEC §6 token contract, part 3).
const QUAL_PATH: i64 = Q;

/// An existing absolute path typed as the query ranks its target first (guarantee a).
const EXISTING_PATH: i64 = 100 * Q;
/// A query matching an alias word recommends its target as a top result (guarantee g).
const ALIAS_RECOMMEND: i64 = 50 * Q;

/// Added when a path-shaped match falls under the typed prefix (guarantee c). Below a
/// quality band but above every penalty and boost, so scoped matches win within a band.
const SCOPE_BONUS: i64 = 500_000;
/// A layout-corrected match ranks just below a same-quality direct match (guarantee d):
/// larger than every boost combined (boosts can't lift a corrected match past a direct
/// one) yet smaller than a quality band.
const CORRECTION_PENALTY: i64 = 100_000;

/// Junk carries a heavy ranking penalty (guarantee h).
const JUNK_PENALTY: i64 = 40_000;
/// Hidden content carries a light penalty (guarantee h).
const HIDDEN_PENALTY: i64 = 8_000;
/// A Known Place (Home, Desktop, …) is lifted in the results (guarantee f).
const KNOWN_PLACE_BOOST: i64 = 20_000;

/// The physical-key mapping between US QWERTY and РФ ЙЦУКЕН, used for layout correction
/// (guarantee d). No phonetic transliteration — this is the keyboard geometry only.
const LAYOUT_PAIRS: &[(char, char)] = &[
    ('`', 'ё'),
    ('q', 'й'),
    ('w', 'ц'),
    ('e', 'у'),
    ('r', 'к'),
    ('t', 'е'),
    ('y', 'н'),
    ('u', 'г'),
    ('i', 'ш'),
    ('o', 'щ'),
    ('p', 'з'),
    ('[', 'х'),
    (']', 'ъ'),
    ('a', 'ф'),
    ('s', 'ы'),
    ('d', 'в'),
    ('f', 'а'),
    ('g', 'п'),
    ('h', 'р'),
    ('j', 'о'),
    ('k', 'л'),
    ('l', 'д'),
    (';', 'ж'),
    ('\'', 'э'),
    ('z', 'я'),
    ('x', 'ч'),
    ('c', 'с'),
    ('v', 'м'),
    ('b', 'и'),
    ('n', 'т'),
    ('m', 'ь'),
    (',', 'б'),
    ('.', 'ю'),
    ('/', '.'),
];

/// Ranking inputs beyond the index itself: the Visit Journal aggregate and the Alias
/// Dictionary. Both are borrowed for the duration of one search.
pub struct RankContext<'a> {
    pub journal: &'a Aggregate,
    pub aliases: &'a AliasDictionary,
}

impl RankContext<'static> {
    /// A context with no journal and no aliases, for ranker tests and startup's one
    /// content-independent cache warm-up.
    pub fn empty() -> RankContext<'static> {
        use std::sync::OnceLock;
        static AGG: OnceLock<Aggregate> = OnceLock::new();
        static ALIASES: OnceLock<AliasDictionary> = OnceLock::new();
        RankContext {
            journal: AGG.get_or_init(Aggregate::default),
            aliases: ALIASES.get_or_init(AliasDictionary::empty),
        }
    }
}

/// One search result, shaped for the IPC boundary (camelCase to match the TS schema).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    /// `"normal" | "hidden" | "junk"`.
    pub tier: &'static str,
}

struct Candidate {
    score: i64,
    path: String,
    name: String,
    is_directory: bool,
    tier: Tier,
    /// Slot index into [`IndexData::entries`] this candidate came from, so an exhaustive
    /// scan's match set can be cached and re-scanned when the query is later extended.
    slot: u32,
}

#[derive(Clone, Copy)]
struct SearchItem<'a> {
    entry: EntryRef<'a>,
    name_filter: u64,
}

/// A best-effort cancellation token: the search's own generation stamp and the shared
/// counter that a newer `search_name_index` bumps. A shard consults it every
/// [`ABORT_STRIDE`] entries and aborts once the counter has moved past `mine` (SPEC §10: a
/// newer keystroke cancels in-flight work of the previous state).
#[derive(Clone, Copy)]
pub struct Cancel<'a> {
    current: &'a AtomicU64,
    mine: u64,
}

impl<'a> Cancel<'a> {
    /// A token for the search stamped `mine`, watching `current` for a newer stamp.
    pub(crate) fn new(current: &'a AtomicU64, mine: u64) -> Self {
        Self { current, mine }
    }

    fn superseded(&self) -> bool {
        // Relaxed is enough: we only need to eventually observe a newer stamp; correctness
        // never depends on *when* the abort is seen, only that a superseded scan is dropped.
        self.current.load(Ordering::Relaxed) != self.mine
    }
}

/// The full result of one scan: the ranked hits plus the metadata the streaming layer needs
/// to cancel, reuse, and report on it.
pub struct SearchOutcome {
    /// Ranked, deterministic hits (empty when the scan was aborted).
    pub hits: Vec<SearchHit>,
    /// The whole match set was collected — no candidate cap hit, no abort. Only an
    /// exhaustive scan's candidate set is safe to reuse for an extended query.
    pub exhaustive: bool,
    /// A newer query superseded this one mid-scan; `hits` is empty and must be discarded.
    pub aborted: bool,
    /// How many live entries the scan scored — small for a reuse scan, up to the whole
    /// index for a cold full scan; surfaced as search telemetry.
    pub scanned: usize,
    /// Slot indices of the matched entries (only populated when `exhaustive`), cached so an
    /// extended query can rescan just this set instead of the whole index.
    pub candidate_entries: Vec<u32>,
}

/// Everything derived once from the query text, shared read-only across scan shards.
struct Prepared {
    query_lower: String,
    query_name_filter: u64,
    path_shaped: bool,
    segments: Vec<String>,
    segment_name_filters: Vec<u64>,
    /// Whole-query layout-corrected variants (a plain query). For a single-token query these
    /// are the needles [`score_plain`] falls back to; for a multi-token query they are only
    /// the source of [`Prepared::corrected_tokens`].
    corrected: Vec<String>,
    corrected_name_filters: Vec<u64>,
    /// The plain query split on whitespace (SPEC §6 token contract). One element is a
    /// single-token query (scored byte-identically to before tokenization); two or more is a
    /// multi-token AND query. Empty for a path-shaped query (slashes stay literal).
    tokens: Vec<String>,
    /// Each layout-corrected variant, tokenized the same way — populated only for a
    /// multi-token query. Space maps to space, so a variant carries the same token count.
    corrected_tokens: Vec<Vec<String>>,
    /// Flat needle list for the per-dir path mask (multi-token only): the direct token set
    /// followed by each corrected variant's token set, in order. Bit `i` of a dir mask means
    /// `path_needles[i]` occurs in some component of that dir's path. Duplicates across sets
    /// are kept so a flat index maps straight back to a token position.
    path_needles: Vec<String>,
    path_needle_name_filters: Vec<u64>,
    /// Flat-list offset of each corrected variant's token set in [`Prepared::path_needles`]
    /// (the direct set sits at offset 0). `corrected_offsets[k]` is where variant `k`'s tokens
    /// begin, so [`score_multi`] can turn a per-set token position into a flat needle index.
    corrected_offsets: Vec<usize>,
    /// Whether the path check uses the per-dir bit-mask cache: multi-token and at most 63
    /// needles (a `u64` mask with one marker bit). A pasted wall of text can exceed 63 needles; that overflow
    /// falls back to the uncached ancestor walk instead (same component-wise test).
    use_path_mask: bool,
    /// Lowercased `Normal` components of the index root, for the overflow-fallback path check
    /// only — the mask path folds the root's bits into `mask(0)`. Populated only for
    /// multi-token queries.
    root_lower: Vec<String>,
    known: HashSet<PathBuf>,
}

/// A `u64` mask holds one bit per path needle, so beyond this many needles the per-dir mask
/// cache cannot represent the answer and the scan falls back to the uncached ancestor walk.
/// Bit 63 is reserved as the cached marker in [`DirMaskCache`], leaving 63 query bits. A query
/// large enough to exceed that still takes the semantics-identical ancestor-walk fallback.
const MAX_PATH_NEEDLES: usize = 63;

impl Prepared {
    fn new(index: &IndexData, trimmed: &str, query_lower: &str) -> Self {
        let known = known_places(&index.root);
        let path_shaped = is_path_shaped(trimmed);
        let segments = if path_shaped {
            path_segments(query_lower)
        } else {
            Vec::new()
        };
        let segment_name_filters = segments
            .iter()
            .map(|segment| name_filter(segment))
            .collect();
        // Layout correction applies to plain queries; a path-shaped query already carries
        // structure that the wrong layout would not have produced.
        let corrected = if path_shaped {
            Vec::new()
        } else {
            layout_variants(query_lower)
        };
        let corrected_name_filters = corrected
            .iter()
            .map(|variant| name_filter(variant))
            .collect();
        // A plain query tokenizes on whitespace; a path-shaped one keeps spaces literal
        // (paths can contain spaces), so it is never tokenized.
        let tokens = if path_shaped {
            Vec::new()
        } else {
            tokenize(query_lower)
        };
        let multi = tokens.len() > 1;
        let corrected_tokens: Vec<Vec<String>> = if multi {
            corrected.iter().map(|variant| tokenize(variant)).collect()
        } else {
            Vec::new()
        };
        // Flatten the direct token set and every corrected set into one needle list, recording
        // where each corrected set begins so a (set, position) pair maps to a flat bit index.
        let mut path_needles = Vec::new();
        let mut corrected_offsets = Vec::new();
        if multi {
            path_needles.extend(tokens.iter().cloned());
            for set in &corrected_tokens {
                corrected_offsets.push(path_needles.len());
                path_needles.extend(set.iter().cloned());
            }
        }
        let use_path_mask = multi && path_needles.len() <= MAX_PATH_NEEDLES;
        let path_needle_name_filters = path_needles
            .iter()
            .map(|needle| name_filter(needle))
            .collect();
        let root_lower = if multi {
            root_components_lower(&index.root)
        } else {
            Vec::new()
        };
        Self {
            query_lower: query_lower.to_owned(),
            query_name_filter: name_filter(query_lower),
            path_shaped,
            segments,
            segment_name_filters,
            corrected,
            corrected_name_filters,
            tokens,
            corrected_tokens,
            path_needles,
            path_needle_name_filters,
            corrected_offsets,
            use_path_mask,
            root_lower,
            known,
        }
    }

    /// Whether an Item name can satisfy a name-only query before its heap string is read.
    /// Multi-token queries may match missing tokens through the path, so they cannot reject
    /// the whole Item here and use the same filter later inside `token_quality` instead.
    fn name_only_may_match(&self, item_filter: u64) -> bool {
        if self.path_shaped {
            return self
                .segment_name_filters
                .last()
                .is_some_and(|query_filter| filter_contains(item_filter, *query_filter));
        }
        if self.tokens.len() > 1 {
            return true;
        }
        filter_contains(item_filter, self.query_name_filter)
            || self
                .corrected_name_filters
                .iter()
                .any(|query_filter| filter_contains(item_filter, *query_filter))
    }
}

#[inline]
fn filter_contains(item_filter: u64, query_filter: u64) -> bool {
    item_filter & query_filter == query_filter
}

/// Scan-local memo answering, per directory, the one bit a multi-token path check needs:
/// "does needle `i` occur (case-insensitive) in some component of this dir's path?". Bit `i`
/// of a dir's mask is set iff [`Prepared::path_needles`]`[i]` occurs in some component of that
/// dir's path, the index root's own components included:
///   `mask(d) = mask(parent) | bits_of(name(d))`, with `mask(0)` the bits of the root path's
///   `Normal` components (the old `root_components_lower` semantics).
///
/// One `u64` per directory node, sized to `index.nodes.len()`. Bit 63 marks a built slot and the
/// lower 63 bits answer the query needles. This is half the memory of `Option<u64>` on the
/// reference target and halves the zero-fill paid by every search shard. The index is immutable
/// under the read lock for the duration of a search, so a cached mask never goes stale within
/// the scan; each shard (and the reuse scan) owns one — the cache is never shared across threads
/// and never stored in [`IndexData`].
///
/// Millions of entries share the same few hundred thousand parent directories, so memoizing
/// the answer per dir turns the per-entry ancestor walk into one `Vec` index and one bit test;
/// each dir's own name is tested against each needle at most once per scan.
struct DirMaskCache {
    masks: Vec<u64>,
}

impl DirMaskCache {
    const CACHED: u64 = 1u64 << 63;

    fn new(node_count: usize) -> Self {
        Self {
            masks: vec![0; node_count],
        }
    }

    /// The needle-occurrence mask for `dir`, building and memoizing it (and any uncached
    /// ancestors) on first use.
    fn mask_for(
        &mut self,
        index: &IndexData,
        dir: DirId,
        needles: &[String],
        scratch: &mut String,
    ) -> u64 {
        let cached = self.masks[dir as usize];
        if cached & Self::CACHED != 0 {
            return cached & !Self::CACHED;
        }
        let mask = self.build(index, dir, needles, scratch);
        debug_assert_eq!(mask & Self::CACHED, 0);
        self.masks[dir as usize] = mask | Self::CACHED;
        mask
    }

    fn build(
        &mut self,
        index: &IndexData,
        dir: DirId,
        needles: &[String],
        scratch: &mut String,
    ) -> u64 {
        if dir == 0 {
            // Root: bits of needles occurring in the root path's own `Normal` components.
            let mut mask = 0u64;
            for component in index.root.components() {
                if let Component::Normal(name) = component {
                    mask |= component_bits(&name.to_string_lossy(), needles, scratch);
                }
            }
            return mask;
        }
        let parent = index.node(dir).expect("indexed directory node").parent;
        // Recurse first, then borrow this node's name. Borrowing after the recursive mutable
        // `self` call avoids cloning the boxed component — on the reference index that clone
        // meant hundreds of thousands of tiny allocations during every cold multi-token scan.
        let parent_mask = self.mask_for(index, parent, needles, scratch);
        parent_mask
            | component_bits(
                index.node(dir).expect("indexed directory node").name,
                needles,
                scratch,
            )
    }
}

/// A fresh mask cache for one scan, or `None` when the query does not use it: single-token
/// and path-shaped queries never do the path-mask check, and a multi-token query with more
/// than 63 needles overflows the cache and takes the uncached ancestor-walk fallback instead.
/// Sizing the cache to the node count is skipped in those cases, so a single-token scan pays
/// nothing for it.
fn new_dir_masks(index: &IndexData, prep: &Prepared) -> Option<DirMaskCache> {
    prep.use_path_mask
        .then(|| DirMaskCache::new(index.node_len()))
}

/// The bits of `needles` that occur (case-insensitive substring) in one path `component`.
/// A single component drives one `contains_ci` per needle, allocation-free for ASCII names.
fn component_bits(component: &str, needles: &[String], scratch: &mut String) -> u64 {
    let mut bits = 0u64;
    for (i, needle) in needles.iter().enumerate() {
        if contains_ci(component, needle, scratch) {
            bits |= 1u64 << i;
        }
    }
    bits
}

/// Run a search, returning up to `limit` hits in the §6 ranked, deterministic order — the
/// whole-index full-scan entry the ranker-contract tests exercise. Production searches go
/// through [`run`] (via `mod.rs`), which adds cancellation and reuse.
#[cfg(test)]
pub fn search(index: &IndexData, ctx: &RankContext, query: &str, limit: usize) -> Vec<SearchHit> {
    run(index, ctx, query, limit, None, None).hits
}

/// Run a search with optional stale-cancellation and optional candidate reuse.
///
/// `reuse` restricts the scan to a previous exhaustive scan's match set (typing-extension
/// reuse); `None` scans the whole index in parallel. The ranked order is byte-identical to a
/// full scan for the same query either way, because prefix/substring/exact matching only
/// *shrinks* the match set under query extension: a reuse scan therefore sees a complete
/// superset of the extended query's matches (the reuse gate that guarantees this lives at
/// the `can_extend` call site in `mod.rs`).
pub fn run(
    index: &IndexData,
    ctx: &RankContext,
    query: &str,
    limit: usize,
    reuse: Option<&[u32]>,
    cancel: Option<&Cancel>,
) -> SearchOutcome {
    // A reuse scan touches at most `MAX_CANDIDATES` entries, so it never pays for threads; a
    // full scan splits across cores only once the index is large enough to be worth it.
    let shards = if reuse.is_some() {
        1
    } else {
        resolve_shards(index.slot_len())
    };
    run_impl(index, ctx, query, limit, reuse, cancel, shards)
}

/// The scan core, with an explicit shard count (the benchmark forces `1` for a sequential
/// baseline; production resolves it from the core count). Sharding never changes the output:
/// the candidate cap keeps the first `MAX_CANDIDATES` matches *in index order* regardless of
/// how the range was split, and the final total-order sort is independent of merge order.
pub(crate) fn run_impl(
    index: &IndexData,
    ctx: &RankContext,
    query: &str,
    limit: usize,
    reuse: Option<&[u32]>,
    cancel: Option<&Cancel>,
    shards: usize,
) -> SearchOutcome {
    let trimmed = query.trim();
    let query_lower = trimmed.to_lowercase();
    if query_lower.is_empty() || limit == 0 {
        return SearchOutcome {
            hits: Vec::new(),
            exhaustive: true,
            aborted: false,
            scanned: 0,
            candidate_entries: Vec::new(),
        };
    }

    // An exact existing path is already the strongest possible answer (guarantee a).
    // Resolve it before touching the Name Index: scanning millions of entries only to
    // inject this same path afterwards turns direct navigation into a multi-second search.
    // Mark the shortcut non-exhaustive so its singleton result is never reused as a match
    // set for a later, extended query.
    if is_path_shaped(trimmed) {
        if let Some((target, is_dir)) = existing_typed_path(trimmed, &index.root) {
            let mut candidates = Vec::with_capacity(1);
            inject(&mut candidates, index, &target, EXISTING_PATH, is_dir);
            let hits = candidates
                .into_iter()
                .map(|candidate| SearchHit {
                    name: candidate.name,
                    path: candidate.path,
                    is_directory: candidate.is_directory,
                    tier: candidate.tier.as_str(),
                })
                .collect();
            return SearchOutcome {
                hits,
                exhaustive: false,
                aborted: false,
                scanned: 0,
                candidate_entries: Vec::new(),
            };
        }
    }

    let prep = Prepared::new(index, trimmed, &query_lower);
    let (mut candidates, scanned, aborted, capped) = match reuse {
        Some(slots) => scan_reuse(index, &prep, ctx, slots, cancel),
        None => scan_full(index, &prep, ctx, cancel, shards),
    };

    // A superseded scan returns nothing; the streaming layer turns this into the stale
    // signal the frontend's seq-guard already drops.
    if aborted {
        return SearchOutcome {
            hits: Vec::new(),
            exhaustive: false,
            aborted: true,
            scanned,
            candidate_entries: Vec::new(),
        };
    }

    let exhaustive = !capped;
    // The reusable match set is the scanned candidates *before* any injection: the existing
    // path and alias injections below are functions of the query, not the scan (guarantee a
    // / g stay outside the parallel region), and are re-derived fresh on every search.
    let candidate_entries = if exhaustive {
        candidates.iter().map(|candidate| candidate.slot).collect()
    } else {
        Vec::new()
    };

    // Guarantee (g): an alias word recommends its target — added, never used to filter.
    if let Some(target) = ctx.aliases.resolve(&query_lower) {
        let target = target.to_path_buf();
        inject(&mut candidates, index, &target, ALIAS_RECOMMEND, true);
    }

    // Higher score first; ties by shorter path, then lexicographic path (determinism).
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.path.len().cmp(&b.path.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
    candidates.truncate(limit);

    let hits = candidates
        .into_iter()
        .map(|candidate| SearchHit {
            name: candidate.name,
            path: candidate.path,
            is_directory: candidate.is_directory,
            tier: candidate.tier.as_str(),
        })
        .collect();
    SearchOutcome {
        hits,
        exhaustive,
        aborted: false,
        scanned,
        candidate_entries,
    }
}

/// Shard count for a full scan of `n` slots: one (inline) below [`PAR_THRESHOLD`], else the
/// available core count.
pub(crate) fn resolve_shards(n: usize) -> usize {
    if n < PAR_THRESHOLD {
        return 1;
    }
    thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
        .clamp(1, MAX_SEARCH_SHARDS)
}

/// Full-index scan, split across `shards` contiguous ranges. Returns the collected
/// candidates (capped at [`MAX_CANDIDATES`], in index order), the number of entries scored,
/// whether the scan was aborted, and whether the candidate cap was hit.
fn scan_full(
    index: &IndexData,
    prep: &Prepared,
    ctx: &RankContext,
    cancel: Option<&Cancel>,
    shards: usize,
) -> (Vec<Candidate>, usize, bool, bool) {
    let n = index.slot_len();
    if shards <= 1 || n < PAR_THRESHOLD {
        let (mut local, scanned, aborted) = scan_range(index, prep, ctx, 0, n, cancel);
        let capped = local.len() >= MAX_CANDIDATES;
        local.truncate(MAX_CANDIDATES);
        return (local, scanned, aborted, capped);
    }

    let chunk = n.div_ceil(shards);
    let parts = thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut lo = 0;
        while lo < n {
            let hi = (lo + chunk).min(n);
            handles.push(scope.spawn(move || {
                crate::qos::set_user_initiated_qos();
                scan_range(index, prep, ctx, lo, hi, cancel)
            }));
            lo = hi;
        }
        handles
            .into_iter()
            .map(|handle| handle.join().expect("search shard panicked"))
            .collect::<Vec<_>>()
    });

    // Merge in shard order (== index order), so `take(MAX_CANDIDATES)` keeps exactly the
    // first matches a single sequential scan would, whatever the core count. Each shard
    // already stops at its own `MAX_CANDIDATES`, so the concatenation is bounded even when
    // the query matches millions of entries.
    let mut all: Vec<Candidate> = Vec::new();
    let mut scanned = 0usize;
    let mut aborted = false;
    for (local, shard_scanned, shard_aborted) in parts {
        scanned += shard_scanned;
        aborted |= shard_aborted;
        all.extend(local);
    }
    // `capped` (== not exhaustive) exactly when a sequential scan would have hit the cap,
    // i.e. total matches ≥ MAX_CANDIDATES — whether concentrated in one shard or spread.
    let capped = all.len() >= MAX_CANDIDATES;
    all.truncate(MAX_CANDIDATES);
    (all, scanned, aborted, capped)
}

/// Scan the half-open slot range `[lo, hi)`, collecting up to [`MAX_CANDIDATES`] local
/// matches (in slot order) and aborting early if a newer query supersedes this one.
fn scan_range(
    index: &IndexData,
    prep: &Prepared,
    ctx: &RankContext,
    lo: usize,
    hi: usize,
    cancel: Option<&Cancel>,
) -> (Vec<Candidate>, usize, bool) {
    let mut local = Vec::new();
    let mut scratch = String::new();
    let mut dir_masks = new_dir_masks(index, prep);
    let mut scanned = 0usize;
    let mut since_check = 0usize;
    for slot in lo..hi {
        since_check += 1;
        if since_check >= ABORT_STRIDE {
            since_check = 0;
            if cancel.is_some_and(Cancel::superseded) {
                return (local, scanned, true);
            }
        }
        let Some(entry) = index.entry(slot) else {
            continue;
        };
        let item_filter = entry.filter;
        scanned += 1;
        if !prep.name_only_may_match(item_filter) {
            continue;
        }
        let item = SearchItem {
            entry,
            name_filter: item_filter,
        };
        if let Some((score, path)) =
            score_entry(index, item, prep, ctx, &mut dir_masks, &mut scratch)
        {
            local.push(Candidate {
                score,
                path,
                name: item.entry.name.to_string(),
                is_directory: item.entry.is_directory,
                tier: item.entry.tier,
                slot: slot as u32,
            });
            if local.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    (local, scanned, false)
}

/// Rescan only a previous exhaustive scan's match set (typing-extension reuse). Single
/// threaded: the set is at most [`MAX_CANDIDATES`] entries.
fn scan_reuse(
    index: &IndexData,
    prep: &Prepared,
    ctx: &RankContext,
    slots: &[u32],
    cancel: Option<&Cancel>,
) -> (Vec<Candidate>, usize, bool, bool) {
    let mut local = Vec::new();
    let mut scratch = String::new();
    let mut dir_masks = new_dir_masks(index, prep);
    let mut scanned = 0usize;
    let mut since_check = 0usize;
    for &slot in slots {
        since_check += 1;
        if since_check >= ABORT_STRIDE {
            since_check = 0;
            if cancel.is_some_and(Cancel::superseded) {
                return (local, scanned, true, false);
            }
        }
        let Some(entry) = index.entry(slot as usize) else {
            continue;
        };
        let item_filter = entry.filter;
        scanned += 1;
        if !prep.name_only_may_match(item_filter) {
            continue;
        }
        let item = SearchItem {
            entry,
            name_filter: item_filter,
        };
        if let Some((score, path)) =
            score_entry(index, item, prep, ctx, &mut dir_masks, &mut scratch)
        {
            local.push(Candidate {
                score,
                path,
                name: item.entry.name.to_string(),
                is_directory: item.entry.is_directory,
                tier: item.entry.tier,
                slot,
            });
            if local.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    let capped = local.len() >= MAX_CANDIDATES;
    local.truncate(MAX_CANDIDATES);
    (local, scanned, false, capped)
}

/// Score one entry against the prepared query, dispatching to the path-shaped or plain
/// scorer. Byte-identical scoring to the pre-parallel inline scan.
fn score_entry(
    index: &IndexData,
    item: SearchItem<'_>,
    prep: &Prepared,
    ctx: &RankContext,
    dir_masks: &mut Option<DirMaskCache>,
    scratch: &mut String,
) -> Option<(i64, String)> {
    if prep.path_shaped {
        score_path_shaped(index, item, prep, ctx, scratch)
    } else if prep.tokens.len() > 1 {
        score_multi(index, item, prep, ctx, dir_masks, scratch)
    } else {
        score_plain(index, item, prep, ctx, scratch)
    }
}

/// Score a plain (non-path) query against one entry, or `None` if it does not match.
/// The name is lowered once; direct and layout-corrected needles reuse that haystack.
fn score_plain(
    index: &IndexData,
    item: SearchItem<'_>,
    prep: &Prepared,
    ctx: &RankContext,
    scratch: &mut String,
) -> Option<(i64, String)> {
    let (mut score, corrected_match) = if let Some(quality) = quality_match(
        item.entry.name,
        item.name_filter,
        &prep.query_lower,
        prep.query_name_filter,
        scratch,
    ) {
        (quality, false)
    } else {
        let mut best: Option<i64> = None;
        for (needle, needle_filter) in prep.corrected.iter().zip(&prep.corrected_name_filters) {
            // A corrected needle can only match a name of the same script: a Cyrillic
            // needle never occurs in an ASCII name, so skip that pair before touching
            // the buffer (keeps the whole-index scan cheap under layout correction).
            if !needle.is_ascii() && item.entry.name.is_ascii() {
                continue;
            }
            if let Some(quality) = quality_match(
                item.entry.name,
                item.name_filter,
                needle,
                *needle_filter,
                scratch,
            ) {
                best = Some(best.map_or(quality, |current| current.max(quality)));
            }
        }
        (best?, true)
    };

    if corrected_match {
        score -= CORRECTION_PENALTY;
    }
    score -= tier_penalty(item.entry.tier);

    let path = index.entry_path(item.entry);
    let path_str = path.to_string_lossy().into_owned();
    score += ctx.journal.boost(&path_str);
    if prep.known.contains(&path) {
        score += KNOWN_PLACE_BOOST;
    }
    Some((score, path_str))
}

/// Score a multi-token plain query against one entry (SPEC §6 token contract): the entry
/// matches only if EVERY token matches it independently, on the name (prefix/substring/exact,
/// exactly the single-needle tiers) or, weaker, anywhere in its path. The hit's quality band
/// is the MINIMUM per-token quality — so a hit where all tokens land in the name outranks one
/// where a token only appears in the path. Structure mirrors [`score_plain`]: the direct
/// token set is tried first and, only if it does not match, the layout-corrected token sets
/// (with the same [`CORRECTION_PENALTY`]). Tokens need not be adjacent and order is irrelevant.
fn score_multi(
    index: &IndexData,
    item: SearchItem<'_>,
    prep: &Prepared,
    ctx: &RankContext,
    dir_masks: &mut Option<DirMaskCache>,
    scratch: &mut String,
) -> Option<(i64, String)> {
    // The direct token set is the flat needle prefix (offset 0); each corrected set follows at
    // its recorded offset, so a per-set token position maps to a flat bit index.
    let (mut score, corrected_match) = if let Some(band) =
        match_token_set(index, item, &prep.tokens, 0, prep, dir_masks, scratch)
    {
        (band, false)
    } else {
        let mut best: Option<i64> = None;
        for (k, set) in prep.corrected_tokens.iter().enumerate() {
            // A corrected token of the other script never matches a same-script name or
            // path; `quality_match`/`contains_ci` reject that mismatch before any copy, so
            // this fallback stays cheap across a whole-index scan (e.g. a Cyrillic corrected
            // token over millions of ASCII names).
            let offset = prep.corrected_offsets[k];
            if let Some(band) = match_token_set(index, item, set, offset, prep, dir_masks, scratch)
            {
                best = Some(best.map_or(band, |current| current.max(band)));
            }
        }
        (best?, true)
    };

    if corrected_match {
        score -= CORRECTION_PENALTY;
    }
    score -= tier_penalty(item.entry.tier);

    let path = index.entry_path(item.entry);
    let path_str = path.to_string_lossy().into_owned();
    score += ctx.journal.boost(&path_str);
    if prep.known.contains(&path) {
        score += KNOWN_PLACE_BOOST;
    }
    Some((score, path_str))
}

/// The quality band of an entry against a whole token set: the minimum per-token quality, or
/// `None` if any token matches neither the name nor the path (the AND fails). Token sets are
/// never empty (a multi-token query has ≥2 tokens, and a corrected variant preserves the
/// count), so `None` here always means an unmatched token, not an empty set.
fn match_token_set(
    index: &IndexData,
    item: SearchItem<'_>,
    tokens: &[String],
    offset: usize,
    prep: &Prepared,
    dir_masks: &mut Option<DirMaskCache>,
    scratch: &mut String,
) -> Option<i64> {
    let mut band = i64::MAX;
    for (j, token) in tokens.iter().enumerate() {
        // `offset + j` is this token's index in `Prepared::path_needles`, i.e. its bit in a
        // dir mask.
        band = band.min(token_quality(
            index,
            item,
            token,
            offset + j,
            prep,
            dir_masks,
            scratch,
        )?);
    }
    (band != i64::MAX).then_some(band)
}

/// The quality of one token against one entry: its name-match tier when the token occurs in
/// the name, else [`QUAL_PATH`] when the token occurs (case-insensitive) in some component of
/// the entry's directory path — an ancestor component or a root component — else `None`. The
/// name is preferred because it is the stronger, higher band; the path is only consulted when
/// the name does not match, so a full-index scan pays for the path check only on entries whose
/// name already missed.
///
/// `flat_index` is the token's index in [`Prepared::path_needles`]. When the mask cache is
/// present the path check is one memoized `Vec` index plus one bit test (`mask & 1<<flat_index`);
/// the `dir_masks == None` branch is the overflow fallback (>63 needles) — the same
/// component-wise test done by an uncached ancestor walk plus the root components, not a second
/// semantics.
fn token_quality(
    index: &IndexData,
    item: SearchItem<'_>,
    token: &str,
    flat_index: usize,
    prep: &Prepared,
    dir_masks: &mut Option<DirMaskCache>,
    scratch: &mut String,
) -> Option<i64> {
    if let Some(quality) = quality_match(
        item.entry.name,
        item.name_filter,
        token,
        prep.path_needle_name_filters[flat_index],
        scratch,
    ) {
        return Some(quality);
    }
    let in_path = match dir_masks {
        Some(cache) => {
            let mask = cache.mask_for(index, item.entry.parent, &prep.path_needles, scratch);
            mask & (1u64 << flat_index) != 0
        }
        None => {
            ancestor_contains(index, item.entry.parent, token, scratch)
                || prep
                    .root_lower
                    .iter()
                    .any(|component| component.contains(token))
        }
    };
    in_path.then_some(QUAL_PATH)
}

/// Split a plain query on whitespace: runs of spaces collapse and leading/trailing space is
/// trimmed (SPEC §6 token contract, part 1). A query with no interior whitespace yields a
/// single token equal to the query itself, so single-token search is unchanged.
fn tokenize(query_lower: &str) -> Vec<String> {
    query_lower.split_whitespace().map(str::to_owned).collect()
}

/// Score a path-shaped query. The last segment matches the entry name; any preceding
/// segments must match ancestor components in order, and when they do the match is under
/// the typed prefix — it gets scope priority and no hidden penalty (guarantee c).
fn score_path_shaped(
    index: &IndexData,
    item: SearchItem<'_>,
    prep: &Prepared,
    ctx: &RankContext,
    scratch: &mut String,
) -> Option<(i64, String)> {
    let (last, prefix) = prep.segments.split_last()?;
    let last_filter = *prep.segment_name_filters.last()?;
    let mut score = quality_match(
        item.entry.name,
        item.name_filter,
        last,
        last_filter,
        scratch,
    )?;

    let scoped = !prefix.is_empty() && ancestors_match(index, item.entry.parent, prefix);
    if scoped {
        score += SCOPE_BONUS;
    }

    // Junk stays penalized even under a typed prefix; only the hidden penalty is waived
    // for a scoped match (guarantee c: "no hidden penalty").
    match item.entry.tier {
        Tier::Junk => score -= JUNK_PENALTY,
        Tier::Hidden if !scoped => score -= HIDDEN_PENALTY,
        _ => {}
    }

    let path = index.entry_path(item.entry);
    let path_str = path.to_string_lossy().into_owned();
    score += ctx.journal.boost(&path_str);
    if prep.known.contains(&path) {
        score += KNOWN_PLACE_BOOST;
    }
    Some((score, path_str))
}

/// Whether the ordered `segments` each occur (as a substring) in the ancestor component
/// names of an entry, in order — i.e. the entry sits under the typed path prefix.
fn ancestors_match(index: &IndexData, parent: DirId, segments: &[String]) -> bool {
    let ancestors = ancestor_names(index, parent);
    let mut next = 0usize;
    for segment in segments {
        loop {
            let Some(name) = ancestors.get(next) else {
                return false;
            };
            next += 1;
            if name.contains(segment) {
                break;
            }
        }
    }
    true
}

/// Whether `needle` occurs (case-insensitive substring) in any ancestor directory component
/// name from `dir` up to the root. Allocation-free for ASCII component names. This is the
/// overflow-fallback path check (>63 needles), used together with the root components in
/// [`token_quality`]; the mask cache folds the same tests into a memoized per-dir bitmask.
fn ancestor_contains(
    index: &IndexData,
    mut dir: DirId,
    needle: &str,
    scratch: &mut String,
) -> bool {
    while dir != 0 {
        let node = index.node(dir).expect("indexed directory node");
        if contains_ci(node.name, needle, scratch) {
            return true;
        }
        dir = node.parent;
    }
    false
}

/// The lowercased `Normal` components of the index root (e.g. `["users", "kiri110k"]`), so a
/// token may match the root prefix of the path as well as the indexed components. Feeds
/// `mask(0)` in the mask cache and the overflow-fallback check in [`token_quality`].
fn root_components_lower(root: &Path) -> Vec<String> {
    root.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect()
}

/// Lowercased component names from the root down to (and including) `dir`.
fn ancestor_names(index: &IndexData, dir: DirId) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = dir;
    while current != 0 {
        let node = index.node(current).expect("indexed directory node");
        names.push(node.name.to_lowercase());
        current = node.parent;
    }
    names.reverse();
    names
}

/// Match quality of the already-lowercased `needle` against a `name`. When both are ASCII
/// (the vast majority of file names) it compares bytes case-insensitively with no
/// allocation; otherwise it Unicode-lowercases `name` into `scratch` and compares.
fn quality_match(
    name: &str,
    item_filter: u64,
    needle: &str,
    needle_filter: u64,
    scratch: &mut String,
) -> Option<i64> {
    if !filter_contains(item_filter, needle_filter) {
        return None;
    }
    if name.is_ascii() && needle.is_ascii() {
        return quality_ascii(name.as_bytes(), needle.as_bytes());
    }
    // A non-ASCII needle (e.g. a layout-corrected Cyrillic token) can never occur in an
    // ASCII name, so reject before copying the name into `scratch`. This is what keeps the
    // corrected-token fallback of a multi-token scan cheap across a whole-index scan of
    // ASCII names (the same short-circuit `score_plain` applies inline before its loop).
    if !needle.is_ascii() && name.is_ascii() {
        return None;
    }
    lower_into(name, scratch);
    quality_of(scratch, needle)
}

/// Match quality of `needle` against an already-lowercased `haystack` (Unicode path).
fn quality_of(haystack: &str, needle: &str) -> Option<i64> {
    if haystack == needle {
        Some(QUAL_EXACT)
    } else if haystack.starts_with(needle) {
        Some(QUAL_PREFIX)
    } else if haystack.contains(needle) {
        Some(QUAL_SUBSTRING)
    } else {
        None
    }
}

/// Case-insensitive ASCII match quality of `needle` (lowercase) against `name` bytes.
fn quality_ascii(name: &[u8], needle: &[u8]) -> Option<i64> {
    if needle.is_empty() || name.len() < needle.len() {
        return None;
    }
    if name.len() == needle.len() && ascii_ci_eq(name, needle) {
        return Some(QUAL_EXACT);
    }
    if ascii_ci_eq(&name[..needle.len()], needle) {
        return Some(QUAL_PREFIX);
    }
    if ascii_contains_ci(name, needle) {
        return Some(QUAL_SUBSTRING);
    }
    None
}

/// Whether two equal-length ASCII byte slices are equal ignoring case.
fn ascii_ci_eq(a: &[u8], b: &[u8]) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Whether the already-lowercased `needle` occurs in `haystack`, case-insensitively. Like
/// [`quality_match`] it takes an allocation-free ASCII byte path (the common case for path
/// components) and only Unicode-lowercases into `scratch` for a non-ASCII haystack.
fn contains_ci(haystack: &str, needle: &str, scratch: &mut String) -> bool {
    if haystack.is_ascii() && needle.is_ascii() {
        return ascii_contains_ci(haystack.as_bytes(), needle.as_bytes());
    }
    // A non-ASCII needle can never occur in an ASCII haystack — skip before touching scratch.
    if !needle.is_ascii() && haystack.is_ascii() {
        return false;
    }
    lower_into(haystack, scratch);
    scratch.contains(needle)
}

/// Case-insensitive ASCII substring test of `needle` (already lowercase) in `haystack` bytes.
fn ascii_contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    // `windows().any(eq_ignore_ascii_case)` checks every possible start byte. Real file names
    // average 33 bytes on the reference machine, so a cold miss repeats that branch over
    // 5M scattered allocations. SIMD `memchr` finds only occurrences of the first byte (in
    // either ASCII case); the full comparison runs at those positions. Same answer, much less
    // work for the overwhelmingly common miss.
    let last_start = haystack.len() - needle.len();
    let starts = &haystack[..=last_start];
    let first_lower = needle[0].to_ascii_lowercase();
    let first_upper = needle[0].to_ascii_uppercase();
    if first_lower == first_upper {
        return memchr_iter(first_lower, starts)
            .any(|start| ascii_ci_eq(&haystack[start..start + needle.len()], needle));
    }
    memchr2_iter(first_lower, first_upper, starts)
        .any(|start| ascii_ci_eq(&haystack[start..start + needle.len()], needle))
}

fn tier_penalty(tier: Tier) -> i64 {
    match tier {
        Tier::Normal => 0,
        Tier::Hidden => HIDDEN_PENALTY,
        Tier::Junk => JUNK_PENALTY,
    }
}

/// Lowercase `name` into the reused `scratch` buffer. The overwhelming majority of file
/// names are ASCII, so a byte-wise fast path avoids the per-`char` Unicode `to_lowercase`
/// iterator — that difference is what keeps the whole-index scan inside the §10 budget on
/// a multi-million-entry index. Non-ASCII names fall back to full Unicode lowercasing.
fn lower_into(name: &str, scratch: &mut String) {
    scratch.clear();
    if name.is_ascii() {
        scratch.push_str(name);
        scratch.make_ascii_lowercase();
        return;
    }
    for ch in name.chars() {
        for lower in ch.to_lowercase() {
            scratch.push(lower);
        }
    }
}

/// A query is path-shaped when it contains a slash or starts with `~` (SPEC §6).
fn is_path_shaped(query: &str) -> bool {
    query.contains('/') || query.starts_with('~')
}

/// Split a path-shaped query into matchable segments, dropping empties and the
/// navigational `.`, `..`, and `~` anchors (they shape ranking, not matching).
fn path_segments(query_lower: &str) -> Vec<String> {
    query_lower
        .split('/')
        .filter_map(|segment| {
            let segment = segment.trim();
            if segment.is_empty() || segment == "." || segment == ".." || segment == "~" {
                None
            } else {
                Some(segment.to_owned())
            }
        })
        .collect()
}

/// The layout-corrected variants of a query (both directions), excluding the query
/// itself. Empty when the query has no keys that differ between layouts.
fn layout_variants(query_lower: &str) -> Vec<String> {
    let mut variants = Vec::new();
    let to_ru = map_layout(query_lower, true);
    if to_ru != query_lower {
        variants.push(to_ru);
    }
    let to_en = map_layout(query_lower, false);
    if to_en != query_lower {
        variants.push(to_en);
    }
    variants
}

/// Remap each character through the physical-key table: `en_to_ru` picks the ЙЦУКЕН
/// character under the same key as a QWERTY character, and the reverse otherwise.
fn map_layout(query: &str, en_to_ru: bool) -> String {
    query
        .chars()
        .map(|ch| {
            LAYOUT_PAIRS
                .iter()
                .find(|(en, ru)| if en_to_ru { *en == ch } else { *ru == ch })
                .map(|(en, ru)| if en_to_ru { *ru } else { *en })
                .unwrap_or(ch)
        })
        .collect()
}

/// The Known Places under the index root (SPEC §6). Mounted volume roots live outside the
/// home root and join this set once volume indexing lands; noted as an open question.
fn known_places(root: &Path) -> HashSet<PathBuf> {
    let mut places = HashSet::new();
    places.insert(root.to_path_buf());
    for name in [
        "Desktop",
        "Documents",
        "Downloads",
        "Movies",
        "Music",
        "Pictures",
        "Public",
    ] {
        places.insert(root.join(name));
    }
    // iCloud Drive root.
    places.insert(root.join("Library/Mobile Documents/com~apple~CloudDocs"));
    places
}

/// Resolve a path-shaped query to an existing absolute path on disk, expanding a leading
/// `~` to the root. Returns the path and whether it is a directory, or `None` when the
/// query is not an absolute/`~` path or nothing exists there.
pub(crate) fn existing_typed_path(query: &str, root: &Path) -> Option<(PathBuf, bool)> {
    let expanded = if query == "~" {
        root.to_path_buf()
    } else if let Some(rest) = query.strip_prefix("~/") {
        root.join(rest)
    } else {
        PathBuf::from(query)
    };
    if !expanded.is_absolute() {
        return None;
    }
    let metadata = std::fs::symlink_metadata(&expanded).ok()?;
    Some((expanded, metadata.is_dir()))
}

/// Add an injected candidate (existing path or alias target) at `score`. If the path is
/// already among the scanned candidates, its score is only lifted, never duplicated. When
/// the path is indexed its real kind and tier are used; otherwise `fallback_is_dir` and
/// the Normal tier stand in.
fn inject(
    candidates: &mut Vec<Candidate>,
    index: &IndexData,
    target: &Path,
    score: i64,
    fallback_is_dir: bool,
) {
    let path_str = target.to_string_lossy().into_owned();
    if let Some(existing) = candidates.iter_mut().find(|c| c.path == path_str) {
        existing.score = existing.score.max(score);
        return;
    }
    let (is_directory, tier) = find_entry(index, target).unwrap_or((fallback_is_dir, Tier::Normal));
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_str.clone());
    candidates.push(Candidate {
        score,
        path: path_str,
        name,
        is_directory,
        tier,
        // Injection happens after the reusable `candidate_entries` are captured, so this
        // slot is never read; the sentinel just marks a candidate that came from the query,
        // not the scan.
        slot: u32::MAX,
    });
}

/// Look up an absolute path in the index, returning `(is_directory, tier)` if present.
fn find_entry(index: &IndexData, path: &Path) -> Option<(bool, Tier)> {
    if path == index.root {
        return Some((true, Tier::Normal));
    }
    let relative = path.strip_prefix(&index.root).ok()?;
    let components: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let (name, parents) = components.split_last()?;

    let mut current: DirId = 0;
    for parent in parents {
        current = index.child_dir(current, parent)?;
    }
    let slot = index.child_slot(current, name)?;
    let entry = index.entry(slot as usize)?;
    Some((entry.is_directory, entry.tier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_index::model::IndexData;

    fn root() -> PathBuf {
        PathBuf::from("/home/tester")
    }

    fn index() -> IndexData {
        IndexData::new(root())
    }

    fn paths(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.path.as_str()).collect()
    }

    fn run(index: &IndexData, query: &str) -> Vec<SearchHit> {
        search(index, &RankContext::empty(), query, 50)
    }

    #[test]
    fn name_filter_keeps_every_supported_substring_match() {
        for (name, needle) in [
            ("README.MD", "readme"),
            ("ПМИ_Процедура_Приемки.html", "процедура"),
            ("Desktop - Kirill’s MacBook Pro", "kirill"),
            ("report-1.42_final.docx", "1.42"),
        ] {
            let needle = needle.to_lowercase();
            let item_filter = name_filter(name);
            let needle_filter = name_filter(&needle);
            assert!(filter_contains(item_filter, needle_filter));
            assert!(quality_match(
                name,
                item_filter,
                &needle,
                needle_filter,
                &mut String::new(),
            )
            .is_some());
        }
    }

    #[test]
    fn exact_beats_penalty_even_for_junk() {
        // An exact-named Junk file must outrank a fuzzy (prefix/substring) normal file
        // (guarantee b).
        let mut index = index();
        index.add_file(0, "config", Tier::Junk); // exact match for "config"
        index.add_file(0, "config-loader.rs", Tier::Normal); // prefix match, normal
        index.add_file(0, "app.config.js", Tier::Normal); // substring match, normal

        let hits = run(&index, "config");
        assert_eq!(hits[0].name, "config");
        assert_eq!(hits[0].tier, "junk");
    }

    #[test]
    fn path_scope_priority_without_hidden_penalty() {
        // secret.txt under the typed prefix `.config` outranks an equally hidden
        // secret.txt elsewhere, because the scoped one is not hidden-penalized
        // (guarantee c).
        let mut index = index();
        let config = index.add_dir(0, ".config", Tier::Hidden, 0);
        index.add_file(config, "secret.txt", Tier::Hidden);
        let other = index.add_dir(0, "other", Tier::Normal, 0);
        index.add_file(other, "secret.txt", Tier::Hidden);

        let hits = run(&index, ".config/secret");
        assert_eq!(hits[0].path, "/home/tester/.config/secret.txt");
        assert_eq!(hits[0].tier, "hidden"); // still hidden — scoped, not excluded
        assert_eq!(hits[1].path, "/home/tester/other/secret.txt");
    }

    #[test]
    fn layout_correction_en_to_ru_ranks_below_direct() {
        // Query "cat" (EN). It prefix-matches "cat.txt" directly, and — mapped to the
        // ЙЦУКЕН keys under c/a/t → "сфе" — prefix-matches "сфе.txt" by correction. The
        // direct match ranks above the corrected one (guarantees d).
        let mut index = index();
        index.add_file(0, "cat.txt", Tier::Normal);
        index.add_file(0, "сфе.txt", Tier::Normal);

        let hits = run(&index, "cat");
        assert_eq!(
            paths(&hits),
            vec!["/home/tester/cat.txt", "/home/tester/сфе.txt",]
        );
    }

    #[test]
    fn layout_correction_ru_to_en_ranks_below_direct() {
        // Query "куку" (RU). Direct-matches "куку.txt"; mapped RU→EN (к/у → r/e) it
        // becomes "rere" and corrects onto "rere.txt".
        let mut index = index();
        index.add_file(0, "куку.txt", Tier::Normal);
        index.add_file(0, "rere.txt", Tier::Normal);

        let hits = run(&index, "куку");
        assert_eq!(
            paths(&hits),
            vec!["/home/tester/куку.txt", "/home/tester/rere.txt",]
        );
    }

    #[test]
    fn visit_boost_changes_order() {
        // Two exact matches; the shorter path wins by default, but a visit boost on the
        // deeper one flips the order (guarantee e).
        let mut index = index();
        index.add_file(0, "notes", Tier::Normal);
        let sub = index.add_dir(0, "sub", Tier::Normal, 0);
        index.add_file(sub, "notes", Tier::Normal);

        let baseline = run(&index, "notes");
        assert_eq!(baseline[0].path, "/home/tester/notes"); // shorter path first

        let aggregate = Aggregate::from_visits(&[("/home/tester/sub/notes", 1_000)]);
        let ctx = RankContext {
            journal: &aggregate,
            aliases: &AliasDictionary::empty(),
        };
        let boosted = search(&index, &ctx, "notes", 50);
        assert_eq!(boosted[0].path, "/home/tester/sub/notes"); // visited path lifted
    }

    #[test]
    fn known_place_boost_lifts_home_location() {
        // Home's Downloads (a Known Place) outranks an all-caps DOWNLOADS file that would
        // otherwise sort first lexicographically (guarantee f).
        let mut index = index();
        index.add_dir(0, "Downloads", Tier::Normal, 0); // Known Place
        index.add_file(0, "DOWNLOADS", Tier::Normal); // sorts before "Downloads" without a boost

        let hits = run(&index, "downloads");
        assert_eq!(hits[0].path, "/home/tester/Downloads");
    }

    #[test]
    fn alias_recommends_target_without_filtering() {
        // "docs" is aliased to ~/Projects; that target surfaces on top, and a literal
        // docs.txt still appears (aliases recommend, never filter — guarantee g).
        let mut index = index();
        index.add_dir(0, "Projects", Tier::Normal, 0);
        index.add_file(0, "docs.txt", Tier::Normal);

        let aliases = AliasDictionary::from_pairs([("docs", "~/Projects")], &root());
        let ctx = RankContext {
            journal: &Aggregate::default(),
            aliases: &aliases,
        };
        let hits = search(&index, &ctx, "docs", 50);
        assert_eq!(hits[0].path, "/home/tester/Projects");
        assert!(hits.iter().any(|hit| hit.path == "/home/tester/docs.txt"));
    }

    #[test]
    fn hidden_and_junk_penalty_ordering() {
        // Same prefix-match quality; normal > hidden > junk (guarantee h).
        let mut index = index();
        index.add_file(0, "report-normal", Tier::Normal);
        index.add_file(0, "report-hidden", Tier::Hidden);
        index.add_file(0, "report-junk", Tier::Junk);

        let hits = run(&index, "report");
        assert_eq!(hits[0].name, "report-normal");
        assert_eq!(hits[1].name, "report-hidden");
        assert_eq!(hits[2].name, "report-junk");
    }

    #[test]
    fn deterministic_tie_breaks_by_length_then_lexicographic() {
        // Identical quality and tier: shorter path first, then lexicographic.
        let mut index = index();
        let b = index.add_dir(0, "b", Tier::Normal, 0);
        index.add_file(b, "item", Tier::Normal); // /home/tester/b/item (longer)
        index.add_file(0, "item", Tier::Normal); // /home/tester/item (shorter)
        let a = index.add_dir(0, "a", Tier::Normal, 0);
        index.add_file(a, "item", Tier::Normal); // /home/tester/a/item

        let hits = run(&index, "item");
        assert_eq!(
            paths(&hits),
            vec![
                "/home/tester/item",   // shortest
                "/home/tester/a/item", // same length as b/item, lexicographically first
                "/home/tester/b/item",
            ]
        );
    }

    #[test]
    fn reuse_matches_full_scan_on_extension() {
        // Extending a query can only shrink the match set (prefix/substring/exact), so
        // rescanning the previous exhaustive candidate set yields hits byte-identical to a
        // fresh full scan — the reuse optimization never changes results.
        let mut index = index();
        index.add_file(0, "report.txt", Tier::Normal);
        index.add_file(0, "report-2024.txt", Tier::Normal);
        index.add_file(0, "quarterly-report.md", Tier::Normal);
        index.add_file(0, "unrelated.txt", Tier::Normal);

        let ctx = RankContext::empty();
        let base = super::run(&index, &ctx, "report", 50, None, None);
        assert!(base.exhaustive);

        let full = super::run(&index, &ctx, "report-", 50, None, None);
        let reused = super::run(
            &index,
            &ctx,
            "report-",
            50,
            Some(&base.candidate_entries),
            None,
        );
        assert!(reused.exhaustive);
        assert_eq!(reused.hits, full.hits);
        assert!(!reused.hits.is_empty());
    }

    #[test]
    fn reuse_covers_layout_corrected_extension() {
        // The layout-corrected variant of an extended query extends the prefix's corrected
        // variant (the key map is per-character), so a corrected match of the extended query
        // is always present in the prefix's cached candidate set.
        let mut index = index();
        index.add_file(0, "сфе.txt", Tier::Normal); // "cat" mapped EN->RU corrects onto this
        index.add_file(0, "unrelated.txt", Tier::Normal);

        let ctx = RankContext::empty();
        // "ca" -> corrected "сф"; base matches "сфе.txt" via the corrected substring.
        let base = super::run(&index, &ctx, "ca", 50, None, None);
        assert!(base.exhaustive);
        assert!(base.hits.iter().any(|hit| hit.name == "сфе.txt"));

        // Extend to "cat" -> corrected "сфе"; still matches "сфе.txt", and reusing the "ca"
        // candidate set finds it just as a full scan does.
        let full = super::run(&index, &ctx, "cat", 50, None, None);
        let reused = super::run(&index, &ctx, "cat", 50, Some(&base.candidate_entries), None);
        assert_eq!(reused.hits, full.hits);
        assert!(reused.hits.iter().any(|hit| hit.name == "сфе.txt"));
    }

    #[test]
    fn multi_token_finds_separator_joined_name() {
        // The #26 repro: a space-separated query finds a name whose words are joined by a
        // separator (here `_`), and ranks it above a hit where a token only matches the path.
        let mut index = index();
        let work = index.add_dir(0, "work", Tier::Normal, 0);
        let vault = index.add_dir(work, "vault", Tier::Normal, 0);
        let reports = index.add_dir(vault, "reports", Tier::Normal, 0);
        // Target: both tokens occur in the name (band = substring quality).
        index.add_file(reports, "ПМИ_процедура_приемки.html", Tier::Normal);
        // Decoy A: "процедура" in the name, "приемки" only in an ancestor dir (weaker band).
        let priemki = index.add_dir(work, "приемки", Tier::Normal, 0);
        index.add_file(priemki, "процедура.txt", Tier::Normal);
        // Decoy B: only one of the two tokens occurs anywhere — the AND excludes it.
        index.add_file(work, "процедура_отчет.txt", Tier::Normal);

        let hits = run(&index, "процедура приемки");
        assert_eq!(hits[0].name, "ПМИ_процедура_приемки.html");
        assert!(hits.iter().any(|hit| hit.name == "процедура.txt")); // present, ranked lower
        assert!(!hits.iter().any(|hit| hit.name == "процедура_отчет.txt")); // one token: excluded
    }

    #[test]
    fn multi_token_finds_via_wrong_layout() {
        // The same query typed on the EN layout (physical keys of «процедура приемки») still
        // finds the target, corrected whole-query-then-tokenized (space maps to space).
        let mut index = index();
        let reports = index.add_dir(0, "reports", Tier::Normal, 0);
        index.add_file(reports, "ПМИ_процедура_приемки.html", Tier::Normal);

        let typed = map_layout("процедура приемки", false); // RU letters → the EN keys under them
        assert!(typed.is_ascii() && typed.contains(' '));
        let hits = run(&index, &typed);
        assert_eq!(hits[0].name, "ПМИ_процедура_приемки.html");
    }

    #[test]
    fn multi_token_one_token_matches_path_only() {
        // A token that never occurs in the name still matches through the path (an ancestor
        // directory), while the other token matches the name.
        let mut index = index();
        let invoices = index.add_dir(0, "invoices", Tier::Normal, 0);
        index.add_file(invoices, "report_2024.txt", Tier::Normal);

        let hits = run(&index, "invoices report");
        assert!(hits.iter().any(|hit| hit.name == "report_2024.txt"));
    }

    #[test]
    fn multi_token_order_is_irrelevant() {
        let mut index = index();
        let invoices = index.add_dir(0, "invoices", Tier::Normal, 0);
        index.add_file(invoices, "report_2024.txt", Tier::Normal);

        assert_eq!(
            run(&index, "invoices report"),
            run(&index, "report invoices")
        );
    }

    #[test]
    fn multi_token_finds_real_spaced_name_and_separator_variant() {
        // A file whose name really contains a space is still found by its spaced substring,
        // and the same query now also finds the underscore-joined variant (the #26 fix).
        let mut index = index();
        index.add_file(0, "annual report final.pdf", Tier::Normal);
        index.add_file(0, "annual_report_draft.pdf", Tier::Normal);

        let hits = run(&index, "annual report");
        assert!(hits.iter().any(|hit| hit.name == "annual report final.pdf"));
        assert!(hits.iter().any(|hit| hit.name == "annual_report_draft.pdf"));
    }

    #[test]
    fn single_token_scoring_is_unchanged_by_tokenization() {
        // A one-word query has exactly one token and must route through the untouched plain
        // scorer: prefix before substring, shorter path first — byte-identical to before.
        let mut index = index();
        index.add_file(0, "report.txt", Tier::Normal);
        index.add_file(0, "annual-report.txt", Tier::Normal);
        let hits = run(&index, "report");
        assert_eq!(hits[0].name, "report.txt"); // prefix beats substring
        assert_eq!(hits[1].name, "annual-report.txt");
    }

    #[test]
    fn parallel_scan_matches_sequential() {
        // A large index forces multi-shard scanning; the ranked output must be identical to a
        // single-shard (sequential) scan — sharding never reorders or drops results.
        let mut index = index();
        for d in 0..70u32 {
            let dir = index.add_dir(0, &format!("dir{d:03}"), Tier::Normal, 0);
            for f in 0..1000u32 {
                index.add_file(dir, &format!("f{d:03}_{f:04}.txt"), Tier::Normal);
            }
        }
        assert!(index.slot_len() > super::PAR_THRESHOLD);

        let ctx = RankContext::empty();
        // A selective query with matches that fall in a late shard.
        let sequential = super::run_impl(&index, &ctx, "f069_0500", 50, None, None, 1);
        let parallel = super::run_impl(&index, &ctx, "f069_0500", 50, None, None, 8);
        assert_eq!(sequential.hits, parallel.hits);
        assert!(!sequential.hits.is_empty());

        // A cap-hitting common query (matches everything) is also identical across shard
        // counts: the cap keeps the first MAX_CANDIDATES in index order either way.
        let seq_common = super::run_impl(&index, &ctx, "f", 50, None, None, 1);
        let par_common = super::run_impl(&index, &ctx, "f", 50, None, None, 8);
        assert_eq!(seq_common.hits, par_common.hits);
        assert!(!seq_common.exhaustive); // the cap was hit
    }

    #[test]
    fn superseded_scan_aborts() {
        // Enough entries to cross an ABORT_STRIDE checkpoint, where the stale stamp is seen.
        let mut index = index();
        for f in 0..(super::ABORT_STRIDE as u32 + 500) {
            index.add_file(0, &format!("file{f:05}.txt"), Tier::Normal);
        }
        let ctx = RankContext::empty();
        // Stamp the scan behind the shared counter: it is superseded at the first check.
        let generation = AtomicU64::new(7);
        let cancel = super::Cancel::new(&generation, 1);
        let out = super::run_impl(&index, &ctx, "file", 50, None, Some(&cancel), 1);
        assert!(out.aborted);
        assert!(out.hits.is_empty());
    }
}
