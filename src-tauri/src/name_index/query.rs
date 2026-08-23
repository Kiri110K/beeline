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

use serde::Serialize;

use crate::name_index::{
    alias::AliasDictionary,
    model::{DirId, Entry, IndexData, Tier},
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

#[cfg(test)]
impl RankContext<'static> {
    /// A context with no journal and no aliases, for tier/ordering tests and any caller
    /// that only needs the text ranker.
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
    path_shaped: bool,
    segments: Vec<String>,
    corrected: Vec<String>,
    known: HashSet<PathBuf>,
}

impl Prepared {
    fn new(index: &IndexData, trimmed: &str, query_lower: &str) -> Self {
        let known = known_places(&index.root);
        let path_shaped = is_path_shaped(trimmed);
        let segments = if path_shaped {
            path_segments(query_lower)
        } else {
            Vec::new()
        };
        // Layout correction applies to plain queries; a path-shaped query already carries
        // structure that the wrong layout would not have produced.
        let corrected = if path_shaped {
            Vec::new()
        } else {
            layout_variants(query_lower)
        };
        Self {
            query_lower: query_lower.to_owned(),
            path_shaped,
            segments,
            corrected,
            known,
        }
    }
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
        resolve_shards(index.entries.len())
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

    // Guarantee (a): an existing typed absolute path ranks its target first. The fs
    // existence check is only ever done here, for path-shaped queries.
    if prep.path_shaped {
        if let Some((target, is_dir)) = existing_typed_path(trimmed, &index.root) {
            inject(&mut candidates, index, &target, EXISTING_PATH, is_dir);
        }
    }
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
fn resolve_shards(n: usize) -> usize {
    if n < PAR_THRESHOLD {
        return 1;
    }
    thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
        .max(1)
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
    let n = index.entries.len();
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
            handles.push(scope.spawn(move || scan_range(index, prep, ctx, lo, hi, cancel)));
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
        let Some(entry) = &index.entries[slot] else {
            continue;
        };
        scanned += 1;
        if let Some((score, path)) = score_entry(index, entry, prep, ctx, &mut scratch) {
            local.push(Candidate {
                score,
                path,
                name: entry.name.to_string(),
                is_directory: entry.is_directory,
                tier: entry.tier,
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
        let Some(entry) = &index.entries[slot as usize] else {
            continue;
        };
        scanned += 1;
        if let Some((score, path)) = score_entry(index, entry, prep, ctx, &mut scratch) {
            local.push(Candidate {
                score,
                path,
                name: entry.name.to_string(),
                is_directory: entry.is_directory,
                tier: entry.tier,
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
    entry: &Entry,
    prep: &Prepared,
    ctx: &RankContext,
    scratch: &mut String,
) -> Option<(i64, String)> {
    if prep.path_shaped {
        score_path_shaped(index, entry, &prep.segments, &prep.known, ctx, scratch)
    } else {
        score_plain(
            index,
            entry,
            &prep.query_lower,
            &prep.corrected,
            &prep.known,
            ctx,
            scratch,
        )
    }
}

/// Score a plain (non-path) query against one entry, or `None` if it does not match.
/// The name is lowered once; direct and layout-corrected needles reuse that haystack.
fn score_plain(
    index: &IndexData,
    entry: &Entry,
    query_lower: &str,
    corrected: &[String],
    known: &HashSet<PathBuf>,
    ctx: &RankContext,
    scratch: &mut String,
) -> Option<(i64, String)> {
    let (mut score, corrected_match) =
        if let Some(quality) = quality_match(&entry.name, query_lower, scratch) {
            (quality, false)
        } else {
            let mut best: Option<i64> = None;
            for needle in corrected {
                // A corrected needle can only match a name of the same script: a Cyrillic
                // needle never occurs in an ASCII name, so skip that pair before touching
                // the buffer (keeps the whole-index scan cheap under layout correction).
                if !needle.is_ascii() && entry.name.is_ascii() {
                    continue;
                }
                if let Some(quality) = quality_match(&entry.name, needle, scratch) {
                    best = Some(best.map_or(quality, |current| current.max(quality)));
                }
            }
            (best?, true)
        };

    if corrected_match {
        score -= CORRECTION_PENALTY;
    }
    score -= tier_penalty(entry.tier);

    let path = index.entry_path(entry);
    let path_str = path.to_string_lossy().into_owned();
    score += ctx.journal.boost(&path_str);
    if known.contains(&path) {
        score += KNOWN_PLACE_BOOST;
    }
    Some((score, path_str))
}

/// Score a path-shaped query. The last segment matches the entry name; any preceding
/// segments must match ancestor components in order, and when they do the match is under
/// the typed prefix — it gets scope priority and no hidden penalty (guarantee c).
fn score_path_shaped(
    index: &IndexData,
    entry: &Entry,
    segments: &[String],
    known: &HashSet<PathBuf>,
    ctx: &RankContext,
    scratch: &mut String,
) -> Option<(i64, String)> {
    let (last, prefix) = segments.split_last()?;
    let mut score = quality_match(&entry.name, last, scratch)?;

    let scoped = !prefix.is_empty() && ancestors_match(index, entry.parent, prefix);
    if scoped {
        score += SCOPE_BONUS;
    }

    // Junk stays penalized even under a typed prefix; only the hidden penalty is waived
    // for a scoped match (guarantee c: "no hidden penalty").
    match entry.tier {
        Tier::Junk => score -= JUNK_PENALTY,
        Tier::Hidden if !scoped => score -= HIDDEN_PENALTY,
        _ => {}
    }

    let path = index.entry_path(entry);
    let path_str = path.to_string_lossy().into_owned();
    score += ctx.journal.boost(&path_str);
    if known.contains(&path) {
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

/// Lowercased component names from the root down to (and including) `dir`.
fn ancestor_names(index: &IndexData, dir: DirId) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = dir;
    while current != 0 {
        let node = &index.nodes[current as usize];
        names.push(node.name.to_lowercase());
        current = node.parent;
    }
    names.reverse();
    names
}

/// Match quality of the already-lowercased `needle` against a `name`. When both are ASCII
/// (the vast majority of file names) it compares bytes case-insensitively with no
/// allocation; otherwise it Unicode-lowercases `name` into `scratch` and compares.
fn quality_match(name: &str, needle: &str, scratch: &mut String) -> Option<i64> {
    if name.is_ascii() && needle.is_ascii() {
        return quality_ascii(name.as_bytes(), needle.as_bytes());
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
    if name
        .windows(needle.len())
        .any(|window| ascii_ci_eq(window, needle))
    {
        return Some(QUAL_SUBSTRING);
    }
    None
}

/// Whether two equal-length ASCII byte slices are equal ignoring case.
fn ascii_ci_eq(a: &[u8], b: &[u8]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
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
fn existing_typed_path(query: &str, root: &Path) -> Option<(PathBuf, bool)> {
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
        current = *index.nodes[current as usize]
            .child_dirs
            .get(parent.as_str())?;
    }
    if let Some(&dir_id) = index.nodes[current as usize].child_dirs.get(name.as_str()) {
        return Some((true, index.nodes[dir_id as usize].tier));
    }
    for &entry_index in &index.nodes[current as usize].entries {
        if let Some(entry) = &index.entries[entry_index as usize] {
            if &*entry.name == name.as_str() {
                return Some((entry.is_directory, entry.tier));
            }
        }
    }
    None
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
        assert!(index.entries.len() > super::PAR_THRESHOLD);

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
