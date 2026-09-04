//! The ranking contract over the Name Index (SPEC §6).
//!
//! Retrieval and scoring share this module, but every numeric ranking value comes from the
//! one active [`RankerConfig`]. Algorithms produce code-defined evidence; the config maps it
//! into Text Match, Search Memory, General Usage, Context, Alias, Item Kind, and Penalty
//! contributions. The final order is deterministic for the same evidence and fingerprint.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use memchr::{memchr2_iter, memchr_iter};
use serde::{Deserialize, Serialize};

use crate::name_index::{
    alias::AliasDictionary,
    model::{name_filter, DirId, EntryRef, IndexData, Tier},
    ranker_config::{PenaltyWeights, RankerConfig, TextMatchWeights},
    search_memory::MemoryEvidence,
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
/// Name-only fuzzy verification has no per-shard directory-mask allocation, so it can use
/// every core on the reference 10-core Mac to reduce time to the first completed wave.
const MAX_NAME_FUZZY_SHARDS: usize = 10;

/// How many index slots each shard scans between stale-query checks. A few thousand entries
/// is frequent enough to abort a superseded scan promptly (well under a millisecond of extra
/// work at ~tens of millions of entries/s) yet rare enough that the atomic load never shows
/// up in the scan cost.
const ABORT_STRIDE: usize = 4096;

// Every numeric ranking value comes from the active Ranker Configuration. Matching
// algorithms and named features remain code-defined; their relative strength is not.

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
    pub retrieval: RetrievalSignals,
    pub memory: &'a MemoryEvidence,
    pub config: &'a RankerConfig,
}

#[derive(Clone, Copy)]
pub enum RetrievalSource {
    CurrentLocation,
    PinnedAnchor,
    Recents,
}

/// Query-independent Candidate Evidence contributed by Working Set retrieval. Source bits
/// stay distinct so the ranker can apply one weight per pool; an Item present in several
/// pools receives every applicable contribution.
#[derive(Clone, Default)]
pub struct RetrievalSignals {
    by_slot: HashMap<u32, u8>,
}

impl RetrievalSignals {
    pub fn insert(&mut self, slot: u32, source: RetrievalSource) {
        let bit = match source {
            RetrievalSource::CurrentLocation => 1,
            RetrievalSource::PinnedAnchor => 1 << 1,
            RetrievalSource::Recents => 1 << 2,
        };
        *self.by_slot.entry(slot).or_default() |= bit;
    }

    fn boost(&self, slot: u32, config: &RankerConfig) -> i64 {
        let (context, general_usage) = self.contributions(slot, config);
        context.saturating_add(general_usage)
    }

    fn contributions(&self, slot: u32, config: &RankerConfig) -> (i64, i64) {
        let sources = self.by_slot.get(&slot).copied().unwrap_or_default();
        let mut context = 0i64;
        let mut general_usage = 0i64;
        if sources & 1 != 0 {
            context = context.saturating_add(config.context.current_location);
        }
        if sources & (1 << 1) != 0 {
            context = context.saturating_add(config.context.pinned_anchor);
        }
        if sources & (1 << 2) != 0 {
            general_usage = general_usage.saturating_add(config.general_usage.recents);
        }
        (context, general_usage)
    }

    fn contains(&self, slot: u32, source: RetrievalSource) -> bool {
        let bit = match source {
            RetrievalSource::CurrentLocation => 1,
            RetrievalSource::PinnedAnchor => 1 << 1,
            RetrievalSource::Recents => 1 << 2,
        };
        self.by_slot
            .get(&slot)
            .is_some_and(|sources| sources & bit != 0)
    }
}

impl RankContext<'static> {
    /// A context with no journal and no aliases, for ranker tests and startup's one
    /// content-independent cache warm-up.
    pub fn empty() -> RankContext<'static> {
        use std::sync::OnceLock;
        static AGG: OnceLock<Aggregate> = OnceLock::new();
        static ALIASES: OnceLock<AliasDictionary> = OnceLock::new();
        static CONFIG: OnceLock<RankerConfig> = OnceLock::new();
        RankContext {
            journal: AGG.get_or_init(Aggregate::default),
            aliases: ALIASES.get_or_init(AliasDictionary::empty),
            retrieval: RetrievalSignals::default(),
            memory: MemoryEvidence::empty(),
            config: CONFIG.get_or_init(RankerConfig::default),
        }
    }
}

/// One search result, shaped for the IPC boundary (camelCase to match the TS schema).
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    /// `"normal" | "hidden" | "junk"`.
    pub tier: &'static str,
    /// Internal diagnostic value. The UI receives only the presentation fields above;
    /// Ranking Traces retain this score with the active config fingerprint.
    #[serde(skip)]
    pub score: i64,
    #[serde(skip)]
    pub contributions: ScoreContributions,
    #[serde(skip)]
    pub evidence: ScoreEvidence,
}

impl PartialEq for SearchHit {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.path == other.path
            && self.is_directory == other.is_directory
            && self.tier == other.tier
    }
}

impl Eq for SearchHit {}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScoreContributions {
    pub text_match: i64,
    pub search_memory: i64,
    pub general_usage: i64,
    pub context: i64,
    pub alias: i64,
    pub item_kind: i64,
    pub penalties: i64,
}

/// Config-independent evidence retained for the small ranked result set. It is enough to
/// re-apply every production weight without touching the filesystem or Name Index.
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScoreEvidence {
    pub text_match: TextMatchEvidence,
    pub search_memory_milli: i64,
    pub learned_usage_milli: i64,
    pub visit_milli: i64,
    pub current_location: bool,
    pub pinned_anchor: bool,
    pub recents: bool,
    pub known_place: bool,
    pub alias: bool,
    pub item_kind_applied: bool,
    pub is_directory: bool,
    pub tier: String,
    pub tier_penalty_waived: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TextMatchFeature {
    #[default]
    None,
    ExactName,
    PrefixName,
    SubstringName,
    TypoName,
    PathComponent,
    ExistingPath,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TextMatchEvidence {
    pub feature: TextMatchFeature,
    pub typo_edits: u8,
    pub all_tokens_in_name: bool,
    pub path_scope: bool,
    pub layout_corrected: bool,
}

impl TextMatchEvidence {
    fn contribution(&self, config: &RankerConfig) -> i64 {
        self.contribution_with_weights(&config.text_match)
    }

    fn contribution_with_weights(&self, weights: &TextMatchWeights) -> i64 {
        let mut contribution = match self.feature {
            TextMatchFeature::None => 0,
            TextMatchFeature::ExactName => weights.exact_name,
            TextMatchFeature::PrefixName => weights.prefix_name,
            TextMatchFeature::SubstringName => weights.substring_name,
            TextMatchFeature::TypoName => weights.typo_name.saturating_sub(
                i64::from(self.typo_edits).saturating_mul(weights.typo_edit_penalty),
            ),
            TextMatchFeature::PathComponent => weights.path_component,
            TextMatchFeature::ExistingPath => weights.existing_path,
        };
        if self.all_tokens_in_name {
            contribution = contribution.saturating_add(weights.all_tokens_in_name);
        }
        if self.path_scope {
            contribution = contribution.saturating_add(weights.path_scope);
        }
        if self.layout_corrected {
            contribution = contribution.saturating_sub(weights.layout_correction_penalty);
        }
        contribution
    }
}

impl ScoreEvidence {
    /// Re-apply every non-text production weight. `text_match_resolved` is intentionally
    /// carried through until the matcher emits decomposed literal/fuzzy facts too.
    pub fn contributions(&self, config: &RankerConfig) -> ScoreContributions {
        let search_memory = self
            .search_memory_milli
            .saturating_mul(config.search_memory.max)
            / 1_000;
        let learned_usage = self
            .learned_usage_milli
            .saturating_mul(config.search_memory.usage_max)
            / 1_000;
        let visit = self
            .visit_milli
            .saturating_mul(config.general_usage.visit_max)
            / 1_000;
        let recents = i64::from(self.recents).saturating_mul(config.general_usage.recents);
        let current =
            i64::from(self.current_location).saturating_mul(config.context.current_location);
        let pinned = i64::from(self.pinned_anchor).saturating_mul(config.context.pinned_anchor);
        let known = i64::from(self.known_place).saturating_mul(config.context.known_place);
        let alias = i64::from(self.alias).saturating_mul(config.alias.exact);
        let item_kind = if !self.item_kind_applied {
            0
        } else if self.is_directory {
            config.item_kind.directory
        } else {
            config.item_kind.file
        };
        let penalties = if self.tier_penalty_waived {
            0
        } else {
            match self.tier.as_str() {
                "hidden" => config.penalties.hidden.min(config.penalties.total_cap),
                "junk" => config.penalties.junk.min(config.penalties.total_cap),
                _ => 0,
            }
        };
        ScoreContributions {
            text_match: self.text_match.contribution(config),
            search_memory,
            general_usage: learned_usage.saturating_add(visit).saturating_add(recents),
            context: current.saturating_add(pinned).saturating_add(known),
            alias,
            item_kind,
            penalties,
        }
    }

    pub fn weighted_features(&self, config: &RankerConfig) -> Vec<WeightedFeature> {
        let mut features = Vec::with_capacity(12);
        let mut push = |feature: &'static str, raw: i64, normalized_milli: i64, weight: i64| {
            features.push(WeightedFeature {
                feature: feature.to_owned(),
                raw,
                normalized_milli,
                weight,
                contribution: normalized_milli.saturating_mul(weight) / 1_000,
            });
        };
        let (text_feature, text_weight) = match self.text_match.feature {
            TextMatchFeature::None => (None, 0),
            TextMatchFeature::ExactName => {
                (Some("text_match.exact_name"), config.text_match.exact_name)
            }
            TextMatchFeature::PrefixName => (
                Some("text_match.prefix_name"),
                config.text_match.prefix_name,
            ),
            TextMatchFeature::SubstringName => (
                Some("text_match.substring_name"),
                config.text_match.substring_name,
            ),
            TextMatchFeature::TypoName => {
                (Some("text_match.typo_name"), config.text_match.typo_name)
            }
            TextMatchFeature::PathComponent => (
                Some("text_match.path_component"),
                config.text_match.path_component,
            ),
            TextMatchFeature::ExistingPath => (
                Some("text_match.existing_path"),
                config.text_match.existing_path,
            ),
        };
        if let Some(feature) = text_feature {
            push(feature, 1, 1_000, text_weight);
        }
        if self.text_match.typo_edits > 0 {
            push(
                "text_match.typo_edit_penalty",
                i64::from(self.text_match.typo_edits),
                -1_000 * i64::from(self.text_match.typo_edits),
                config.text_match.typo_edit_penalty,
            );
        }
        if self.text_match.all_tokens_in_name {
            push(
                "text_match.all_tokens_in_name",
                1,
                1_000,
                config.text_match.all_tokens_in_name,
            );
        }
        if self.text_match.path_scope {
            push(
                "text_match.path_scope",
                1,
                1_000,
                config.text_match.path_scope,
            );
        }
        if self.text_match.layout_corrected {
            push(
                "text_match.layout_correction_penalty",
                1,
                -1_000,
                config.text_match.layout_correction_penalty,
            );
        }
        if self.search_memory_milli > 0 {
            push(
                "search_memory.association",
                self.search_memory_milli,
                self.search_memory_milli,
                config.search_memory.max,
            );
        }
        if self.learned_usage_milli > 0 {
            push(
                "general_usage.learned",
                self.learned_usage_milli,
                self.learned_usage_milli,
                config.search_memory.usage_max,
            );
        }
        if self.visit_milli > 0 {
            push(
                "general_usage.visits",
                self.visit_milli,
                self.visit_milli,
                config.general_usage.visit_max,
            );
        }
        if self.recents {
            push(
                "general_usage.recents",
                1,
                1_000,
                config.general_usage.recents,
            );
        }
        if self.current_location {
            push(
                "context.current_location",
                1,
                1_000,
                config.context.current_location,
            );
        }
        if self.pinned_anchor {
            push(
                "context.pinned_anchor",
                1,
                1_000,
                config.context.pinned_anchor,
            );
        }
        if self.known_place {
            push("context.known_place", 1, 1_000, config.context.known_place);
        }
        if self.alias {
            push("alias.exact", 1, 1_000, config.alias.exact);
        }
        if self.item_kind_applied {
            let (feature, weight) = if self.is_directory {
                ("item_kind.directory", config.item_kind.directory)
            } else {
                ("item_kind.file", config.item_kind.file)
            };
            push(feature, 1, 1_000, weight);
        }
        let penalty = match self.tier.as_str() {
            "hidden" => Some(("penalties.hidden", config.penalties.hidden)),
            "junk" => Some(("penalties.junk", config.penalties.junk)),
            _ => None,
        };
        if let Some((feature, weight)) = penalty {
            let applied = if self.tier_penalty_waived { 0 } else { -1_000 };
            push(feature, 1, applied, weight.min(config.penalties.total_cap));
        }
        features
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WeightedFeature {
    pub feature: String,
    pub raw: i64,
    pub normalized_milli: i64,
    pub weight: i64,
    pub contribution: i64,
}

impl ScoreContributions {
    pub fn total(&self) -> i64 {
        self.text_match
            .saturating_add(self.search_memory)
            .saturating_add(self.general_usage)
            .saturating_add(self.context)
            .saturating_add(self.alias)
            .saturating_add(self.item_kind)
            .saturating_sub(self.penalties)
    }
}

#[derive(Clone)]
struct Candidate {
    score: i64,
    contributions: ScoreContributions,
    evidence: ScoreEvidence,
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
/// counter that a newer Search v2 request bumps. A shard consults it every
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

    pub(crate) fn superseded(&self) -> bool {
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

pub struct SlotSelection {
    pub slots: Vec<u32>,
    pub aborted: bool,
}

/// Everything derived once from the query text, shared read-only across scan shards.
struct Prepared {
    text: TextMatchWeights,
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
    /// Query-derived bit filters for the prototype candidate rule, prepared once rather
    /// than rebuilding Unicode character masks for every mutable-overlay Item.
    fuzzy_name_filters: Vec<FuzzyNameFilter>,
    known: HashSet<PathBuf>,
}

#[derive(Clone, Copy)]
struct FuzzyNameFilter {
    filter: u64,
    characters: usize,
    missing_bits_per_edit: u32,
}

/// A `u64` mask holds one bit per path needle, so beyond this many needles the per-dir mask
/// cache cannot represent the answer and the scan falls back to the uncached ancestor walk.
/// Bit 63 is reserved as the cached marker in [`DirMaskCache`], leaving 63 query bits. A query
/// large enough to exceed that still takes the semantics-identical ancestor-walk fallback.
const MAX_PATH_NEEDLES: usize = 63;

impl Prepared {
    fn new(index: &IndexData, trimmed: &str, query_lower: &str, config: &RankerConfig) -> Self {
        let known = known_places(&index.root);
        let query_name_filter = name_filter(query_lower);
        let path_shaped = is_path_shaped(trimmed);
        let segments = if path_shaped {
            path_segments(query_lower)
        } else {
            Vec::new()
        };
        let segment_name_filters: Vec<u64> = segments
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
        let corrected_name_filters: Vec<u64> = corrected
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
        let path_needle_name_filters: Vec<u64> = path_needles
            .iter()
            .map(|needle| name_filter(needle))
            .collect();
        let root_lower = if multi {
            root_components_lower(&index.root)
        } else {
            Vec::new()
        };
        let fuzzy_name_filters = if path_shaped {
            segments
                .last()
                .zip(segment_name_filters.last())
                .map(|(token, filter)| prepare_fuzzy_name_filter(token, *filter))
                .into_iter()
                .collect()
        } else if multi {
            path_needles
                .iter()
                .zip(&path_needle_name_filters)
                .map(|(token, filter)| prepare_fuzzy_name_filter(token, *filter))
                .collect()
        } else {
            std::iter::once((query_lower, query_name_filter))
                .chain(
                    corrected
                        .iter()
                        .zip(&corrected_name_filters)
                        .map(|(token, filter)| (token.as_str(), *filter)),
                )
                .map(|(token, filter)| prepare_fuzzy_name_filter(token, filter))
                .collect()
        };
        Self {
            text: config.text_match.clone(),
            query_lower: query_lower.to_owned(),
            query_name_filter,
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
            fuzzy_name_filters,
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

/// Cheap no-false-negative filter for the prototype's candidate contract: an ordinary
/// multi-token result must plausibly match at least one query token in the Item name. The
/// q-gram sidecar enforces that for the immutable base; this keeps a large mutable overlay
/// from falling through to Unicode distance and ancestor reconstruction wholesale.
fn fuzzy_candidate_may_match_name(item_filter: u64, prep: &Prepared, short_fuzzy: bool) -> bool {
    prep.fuzzy_name_filters
        .iter()
        .any(|filter| fuzzy_filter_may_match(item_filter, *filter, short_fuzzy))
}

fn prepare_fuzzy_name_filter(token: &str, filter: u64) -> FuzzyNameFilter {
    let missing_bits_per_edit = token
        .chars()
        .map(|character| {
            let lowered = character.to_lowercase().collect::<String>();
            name_filter(&lowered).count_ones()
        })
        .max()
        .unwrap_or(1);
    FuzzyNameFilter {
        filter,
        characters: token.chars().count(),
        missing_bits_per_edit,
    }
}

fn fuzzy_filter_may_match(
    item_filter: u64,
    query_filter: FuzzyNameFilter,
    short_fuzzy: bool,
) -> bool {
    let edits = allowed_typo_edits(query_filter.characters, short_fuzzy);
    let allowed_missing = query_filter
        .missing_bits_per_edit
        .saturating_mul(edits as u32);
    (query_filter.filter & !item_filter).count_ones() <= allowed_missing
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

/// Verify and rank a q-gram candidate set with bounded Typo Correction. Unlike the legacy
/// exact scan, this path never caps by index order: every supplied candidate reaches ranking.
/// The q-gram sidecar bounds the input before this function runs.
pub fn run_fuzzy(
    index: &IndexData,
    ctx: &RankContext,
    query: &str,
    limit: usize,
    slots: &[u32],
    cancel: Option<&Cancel>,
    short_fuzzy: bool,
) -> SearchOutcome {
    run_fuzzy_inner(index, ctx, query, limit, slots, cancel, short_fuzzy, None)
}

/// Find literal final-token names in the mutable overlay, which is not represented by the
/// q-gram sidecar. This is the overlay half of the ordered-tail early wave; the returned
/// slots still go through full-query verification before the UI sees them.
pub fn ordered_tail_literal_slots(
    index: &IndexData,
    query: &str,
    cancel: Option<&Cancel>,
) -> SlotSelection {
    let trimmed = query.trim();
    let query_lower = trimmed.to_lowercase();
    let prep = Prepared::new(index, trimmed, &query_lower, RankContext::empty().config);
    let mut needles = Vec::new();
    if prep.path_shaped {
        if prep.segments.len() > 1 {
            let last = prep.segments.len() - 1;
            needles.push((
                prep.segments[last].as_str(),
                prep.segment_name_filters[last],
            ));
        }
    } else if prep.tokens.len() > 1 {
        let last = prep.tokens.len() - 1;
        needles.push((
            prep.tokens[last].as_str(),
            prep.path_needle_name_filters[last],
        ));
        for (set, &offset) in prep.corrected_tokens.iter().zip(&prep.corrected_offsets) {
            let last = set.len() - 1;
            needles.push((
                set[last].as_str(),
                prep.path_needle_name_filters[offset + last],
            ));
        }
    }
    if needles.is_empty() {
        return SlotSelection {
            slots: Vec::new(),
            aborted: false,
        };
    }

    let mut slots = Vec::new();
    let mut scratch = String::new();
    for (position, slot) in index.overlay_entry_slots().enumerate() {
        if position.is_multiple_of(ABORT_STRIDE) && cancel.is_some_and(Cancel::superseded) {
            return SlotSelection {
                slots: Vec::new(),
                aborted: true,
            };
        }
        let entry = index
            .entry(slot as usize)
            .expect("live overlay iterator returned a removed entry");
        if needles.iter().any(|(needle, filter)| {
            quality_match(
                entry.name,
                entry.filter,
                needle,
                *filter,
                &mut scratch,
                &prep.text,
            )
            .is_some()
        }) {
            slots.push(slot);
        }
    }
    SlotSelection {
        slots,
        aborted: false,
    }
}

/// The global Search v2 variant: each completed shard can publish the best cumulative
/// ranking collected so far, while the return value remains the deterministic final top K.
#[allow(clippy::too_many_arguments)]
pub fn run_fuzzy_streaming(
    index: &IndexData,
    ctx: &RankContext,
    query: &str,
    limit: usize,
    slots: &[u32],
    cancel: Option<&Cancel>,
    short_fuzzy: bool,
    on_partial: &mut dyn FnMut(Vec<SearchHit>, usize),
) -> SearchOutcome {
    run_fuzzy_inner(
        index,
        ctx,
        query,
        limit,
        slots,
        cancel,
        short_fuzzy,
        Some(on_partial),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_fuzzy_inner(
    index: &IndexData,
    ctx: &RankContext,
    query: &str,
    limit: usize,
    slots: &[u32],
    cancel: Option<&Cancel>,
    short_fuzzy: bool,
    mut on_partial: Option<&mut dyn FnMut(Vec<SearchHit>, usize)>,
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
    let prep = Prepared::new(index, trimmed, &query_lower, ctx.config);
    let shards = resolve_fuzzy_shards(slots.len(), prep.use_path_mask);
    let (mut candidates, scanned, aborted) = if shards <= 1 {
        scan_fuzzy_slots(index, ctx, &prep, slots, cancel, short_fuzzy)
    } else {
        let chunk_size = slots.len().div_ceil(shards);
        let prepared = &prep;
        thread::scope(|scope| {
            let (sender, receiver) = mpsc::channel();
            for chunk in slots.chunks(chunk_size) {
                let sender = sender.clone();
                scope.spawn(move || {
                    crate::qos::set_user_initiated_qos();
                    let part = scan_fuzzy_slots(index, ctx, prepared, chunk, cancel, short_fuzzy);
                    let _ = sender.send(part);
                });
            }
            drop(sender);

            let mut candidates = Vec::new();
            let mut scanned = 0usize;
            let mut aborted = false;
            for (mut local, local_scanned, local_aborted) in receiver {
                rank_fuzzy_candidates(&mut local, limit);
                candidates.extend(local);
                rank_fuzzy_candidates(&mut candidates, limit);
                scanned += local_scanned;
                aborted |= local_aborted;
                if !aborted {
                    if let Some(callback) = on_partial.as_deref_mut() {
                        callback(fuzzy_hits(&candidates), scanned);
                    }
                }
            }
            (candidates, scanned, aborted)
        })
    };
    if aborted {
        return SearchOutcome {
            hits: Vec::new(),
            exhaustive: false,
            aborted: true,
            scanned,
            candidate_entries: Vec::new(),
        };
    }
    if let Some(target) = ctx.aliases.resolve(&query_lower) {
        inject(
            &mut candidates,
            index,
            target,
            ctx.config.alias.exact,
            true,
            InjectedGroup::Alias,
        );
    }
    inject_memory_candidates(&mut candidates, index, ctx);
    rank_fuzzy_candidates(&mut candidates, limit);
    SearchOutcome {
        hits: fuzzy_hits(&candidates),
        exhaustive: true,
        aborted: false,
        scanned,
        candidate_entries: Vec::new(),
    }
}

fn resolve_fuzzy_shards(slots: usize, uses_path_masks: bool) -> usize {
    if slots < PAR_THRESHOLD {
        return 1;
    }
    let maximum = if uses_path_masks {
        MAX_SEARCH_SHARDS
    } else {
        MAX_NAME_FUZZY_SHARDS
    };
    thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1)
        .clamp(1, maximum)
}

fn rank_fuzzy_candidates(candidates: &mut Vec<Candidate>, limit: usize) {
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.len().cmp(&right.path.len()))
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates.dedup_by(|left, right| left.slot == right.slot);
    candidates.truncate(limit);
}

fn fuzzy_hits(candidates: &[Candidate]) -> Vec<SearchHit> {
    candidates
        .iter()
        .map(|candidate| SearchHit {
            name: candidate.name.clone(),
            path: candidate.path.clone(),
            is_directory: candidate.is_directory,
            tier: candidate.tier.as_str(),
            score: candidate.score,
            contributions: candidate.contributions.clone(),
            evidence: candidate.evidence.clone(),
        })
        .collect()
}

fn scan_fuzzy_slots(
    index: &IndexData,
    ctx: &RankContext,
    prep: &Prepared,
    slots: &[u32],
    cancel: Option<&Cancel>,
    short_fuzzy: bool,
) -> (Vec<Candidate>, usize, bool) {
    let mut candidates = Vec::new();
    let mut scratch = String::new();
    let mut fuzzy_scratch = FuzzyScratch::default();
    let mut dir_masks = new_dir_masks(index, prep);
    let mut scanned = 0usize;
    for (position, &slot) in slots.iter().enumerate() {
        if position % ABORT_STRIDE == 0 && cancel.is_some_and(Cancel::superseded) {
            return (candidates, scanned, true);
        }
        let Some(entry) = index.entry(slot as usize) else {
            continue;
        };
        let item = SearchItem {
            entry,
            name_filter: entry.filter,
        };
        if !fuzzy_candidate_may_match_name(item.name_filter, prep, short_fuzzy) {
            continue;
        }
        scanned += 1;
        if let Some((mut score, path, text_evidence)) =
            score_entry(index, item, prep, ctx, &mut dir_masks, &mut scratch).or_else(|| {
                score_fuzzy_entry(index, item, prep, ctx, short_fuzzy, &mut fuzzy_scratch)
            })
        {
            score += ctx.retrieval.boost(slot, ctx.config);
            let (contributions, evidence) = score_contributions(
                score,
                slot,
                &path,
                entry.parent,
                entry.is_directory,
                entry.tier,
                prep,
                ctx,
                index,
                text_evidence,
            );
            candidates.push(Candidate {
                score,
                contributions,
                evidence,
                path,
                name: entry.name.to_owned(),
                is_directory: entry.is_directory,
                tier: entry.tier,
                slot,
            });
        }
    }
    (candidates, scanned, false)
}

fn score_fuzzy_entry(
    index: &IndexData,
    item: SearchItem<'_>,
    prep: &Prepared,
    ctx: &RankContext,
    short_fuzzy: bool,
    fuzzy_scratch: &mut FuzzyScratch,
) -> Option<(i64, String, TextMatchEvidence)> {
    // Every direct, path, and corrected-layout interpretation inspects the same Item name.
    // Keep its lowercase form in the worker's reusable buffer instead of allocating once per
    // interpretation.
    lower_into(item.entry.name, &mut fuzzy_scratch.name_lower);
    let name_lower = fuzzy_scratch.name_lower.as_str();
    // The direct query, its ordered Path Interpretation, and every layout variant all
    // inspect the same directory chain. Lowercase that chain once per Item.
    let ancestors = if !prep.path_shaped && prep.tokens.len() > 1 {
        fuzzy_scratch
            .ancestors
            .lower_names(index, item.entry.parent)
    } else {
        &[]
    };
    let (quality, corrected, path_scope) = if prep.path_shaped {
        let (last, parents) = prep.segments.split_last()?;
        let final_quality = fuzzy_lower_name_quality(
            name_lower,
            last,
            short_fuzzy,
            &mut fuzzy_scratch.edit,
            &prep.text,
        )?;
        let ancestors = if parents.is_empty() {
            &[]
        } else {
            fuzzy_scratch
                .ancestors
                .lower_names(index, item.entry.parent)
        };
        let quality = fuzzy_path_from_final(
            final_quality,
            parents,
            ancestors,
            short_fuzzy,
            &mut fuzzy_scratch.edit,
            &prep.text,
        )?;
        (quality, false, true)
    } else if prep.tokens.len() > 1 {
        let mut best = fuzzy_token_set(
            name_lower,
            &prep.tokens,
            ancestors,
            short_fuzzy,
            &mut fuzzy_scratch.edit,
            &prep.text,
        );
        best = best.max(fuzzy_path_interpretation(
            name_lower,
            &prep.tokens,
            ancestors,
            short_fuzzy,
            &mut fuzzy_scratch.edit,
            &prep.text,
        ));
        let mut corrected = false;
        for tokens in &prep.corrected_tokens {
            let mut candidate = fuzzy_token_set(
                name_lower,
                tokens,
                ancestors,
                short_fuzzy,
                &mut fuzzy_scratch.edit,
                &prep.text,
            );
            candidate = candidate.max(fuzzy_path_interpretation(
                name_lower,
                tokens,
                ancestors,
                short_fuzzy,
                &mut fuzzy_scratch.edit,
                &prep.text,
            ));
            if candidate > best {
                best = candidate;
                corrected = true;
            }
        }
        (best?, corrected, false)
    } else {
        let direct = prep.tokens.first().and_then(|token| {
            fuzzy_lower_name_quality(
                name_lower,
                token,
                short_fuzzy,
                &mut fuzzy_scratch.edit,
                &prep.text,
            )
        });
        let mut best = direct;
        let mut corrected = false;
        for variant in &prep.corrected {
            let candidate = fuzzy_lower_name_quality(
                name_lower,
                variant,
                short_fuzzy,
                &mut fuzzy_scratch.edit,
                &prep.text,
            );
            if candidate > best {
                best = candidate;
                corrected = true;
            }
        }
        (best?, corrected, false)
    };
    let text_evidence = TextMatchEvidence {
        feature: quality.feature,
        typo_edits: quality.typo_edits,
        path_scope,
        layout_corrected: corrected,
        ..TextMatchEvidence::default()
    };
    let mut score = text_evidence.contribution(ctx.config)
        - tier_penalty(item.entry.tier, &ctx.config.penalties);
    let path = index.entry_path(item.entry);
    let path_str = path.to_string_lossy().into_owned();
    score += personal_boost(ctx, &path_str, item.entry.is_directory);
    if prep.known.contains(&path) {
        score += ctx.config.context.known_place;
    }
    Some((score, path_str, text_evidence))
}

fn fuzzy_token_set(
    name_lower: &str,
    tokens: &[String],
    ancestors: &[String],
    short_fuzzy: bool,
    edit_scratch: &mut EditScratch,
    weights: &TextMatchWeights,
) -> Option<MatchQuality> {
    let mut weakest: Option<MatchQuality> = None;
    let mut name_match = false;
    for token in tokens {
        if let Some(quality) =
            fuzzy_lower_name_quality(name_lower, token, short_fuzzy, edit_scratch, weights)
        {
            weakest = Some(weakest.map_or(quality, |current| current.min(quality)));
            name_match = true;
            continue;
        }
        let quality = ancestors
            .iter()
            .filter_map(|component| {
                fuzzy_lower_component_quality(component, token, short_fuzzy, edit_scratch, weights)
            })
            .max()?;
        let quality = quality.cap_as_path(weights);
        weakest = Some(weakest.map_or(quality, |current| current.min(quality)));
    }
    if name_match {
        weakest
    } else {
        None
    }
}

fn fuzzy_path_interpretation(
    name_lower: &str,
    tokens: &[String],
    ancestors: &[String],
    short_fuzzy: bool,
    edit_scratch: &mut EditScratch,
    weights: &TextMatchWeights,
) -> Option<MatchQuality> {
    let (last, parents) = tokens.split_last()?;
    let final_quality =
        fuzzy_lower_name_quality(name_lower, last, short_fuzzy, edit_scratch, weights)?;
    fuzzy_path_from_final(
        final_quality,
        parents,
        ancestors,
        short_fuzzy,
        edit_scratch,
        weights,
    )
}

fn fuzzy_path_from_final(
    final_quality: MatchQuality,
    parents: &[String],
    ancestors: &[String],
    short_fuzzy: bool,
    edit_scratch: &mut EditScratch,
    weights: &TextMatchWeights,
) -> Option<MatchQuality> {
    let mut cursor = ancestors.len();
    let mut weakest = final_quality;
    for token in parents.iter().rev() {
        let mut found = None;
        while cursor > 0 {
            cursor -= 1;
            if let Some(quality) = fuzzy_lower_component_quality(
                &ancestors[cursor],
                token,
                short_fuzzy,
                edit_scratch,
                weights,
            ) {
                found = Some(quality.cap_as_path(weights));
                break;
            }
        }
        weakest = weakest.min(found?);
    }
    Some(weakest)
}

fn fuzzy_lower_name_quality(
    lower: &str,
    query: &str,
    short_fuzzy: bool,
    edit_scratch: &mut EditScratch,
    weights: &TextMatchWeights,
) -> Option<MatchQuality> {
    let stem = lower
        .rsplit_once('.')
        .filter(|(stem, extension)| !stem.is_empty() && !extension.is_empty())
        .map_or(lower, |(stem, _)| stem);
    fuzzy_targets(lower, stem, query, short_fuzzy, edit_scratch, weights)
}

fn fuzzy_lower_component_quality(
    component: &str,
    query: &str,
    short_fuzzy: bool,
    edit_scratch: &mut EditScratch,
    weights: &TextMatchWeights,
) -> Option<MatchQuality> {
    // `ancestor_names` already lowercases every component once per candidate.
    fuzzy_targets(
        component,
        component,
        query,
        short_fuzzy,
        edit_scratch,
        weights,
    )
}

fn fuzzy_targets(
    full: &str,
    stem: &str,
    query: &str,
    short_fuzzy: bool,
    edit_scratch: &mut EditScratch,
    weights: &TextMatchWeights,
) -> Option<MatchQuality> {
    let edits = allowed_typo_edits(query.chars().count(), short_fuzzy);
    if edits == 0 {
        return None;
    }
    let mut best = None;
    for target in full
        .split(|character: char| !character.is_alphanumeric())
        .chain(std::iter::once(stem))
        .filter(|target| !target.is_empty())
    {
        if let Some(distance) = edit_scratch.distance(query, target, edits) {
            let score = weights
                .typo_name
                .saturating_sub(distance as i64 * weights.typo_edit_penalty);
            let quality = MatchQuality {
                score,
                feature: TextMatchFeature::TypoName,
                typo_edits: u8::try_from(distance).unwrap_or(u8::MAX),
            };
            best = Some(best.map_or(quality, |current: MatchQuality| {
                if quality.score > current.score {
                    quality
                } else {
                    current
                }
            }));
        }
    }
    best
}

fn allowed_typo_edits(length: usize, short_fuzzy: bool) -> usize {
    match length {
        0 => 0,
        1..=2 if short_fuzzy => 1,
        1..=2 => 0,
        3..=6 => 1,
        7..=12 => 2,
        _ => 3,
    }
}

#[derive(Default)]
struct EditScratch {
    left_chars: Vec<char>,
    right_chars: Vec<char>,
    rows: EditRows,
}

#[derive(Default)]
struct FuzzyScratch {
    name_lower: String,
    edit: EditScratch,
    ancestors: AncestorScratch,
}

#[derive(Default)]
struct AncestorScratch {
    prepared_dir: Option<DirId>,
    dirs: Vec<DirId>,
    names: Vec<String>,
}

impl AncestorScratch {
    /// Return lowercase component names from the root down to `dir`, retaining every backing
    /// allocation for the worker's next candidate.
    fn lower_names(&mut self, index: &IndexData, mut dir: DirId) -> &[String] {
        if self.prepared_dir == Some(dir) {
            return &self.names[..self.dirs.len()];
        }
        self.prepared_dir = Some(dir);
        self.dirs.clear();
        while dir != 0 {
            self.dirs.push(dir);
            dir = index.node(dir).expect("indexed directory node").parent;
        }
        self.dirs.reverse();

        while self.names.len() < self.dirs.len() {
            self.names.push(String::new());
        }
        for (&dir, name) in self.dirs.iter().zip(&mut self.names) {
            let node = index.node(dir).expect("indexed directory node");
            lower_into(node.name, name);
        }
        &self.names[..self.dirs.len()]
    }
}

impl EditScratch {
    fn distance(&mut self, left: &str, right: &str, limit: usize) -> Option<usize> {
        if left.is_ascii() && right.is_ascii() {
            return self.rows.distance(left.as_bytes(), right.as_bytes(), limit);
        }
        self.left_chars.clear();
        self.left_chars.extend(left.chars());
        self.right_chars.clear();
        self.right_chars.extend(right.chars());
        self.rows
            .distance(&self.left_chars, &self.right_chars, limit)
    }
}

#[derive(Default)]
struct EditRows {
    previous_two: Vec<u16>,
    previous: Vec<u16>,
    current: Vec<u16>,
}

impl EditRows {
    /// Banded optimal-string-alignment distance. Only cells within `limit` of the main
    /// diagonal can contribute to an accepted answer; all other cells are represented by
    /// the `limit + 1` sentinel. The three rows retain their allocation for the worker's
    /// next candidate.
    fn distance<T: Eq>(&mut self, left: &[T], right: &[T], limit: usize) -> Option<usize> {
        if left.len().abs_diff(right.len()) > limit {
            return None;
        }
        if left.is_empty() || right.is_empty() {
            let distance = left.len().max(right.len());
            return (distance <= limit).then_some(distance);
        }
        let width = right.len() + 1;
        let outside = u16::try_from(limit.saturating_add(1)).unwrap_or(u16::MAX);
        self.previous_two.resize(width, outside);
        self.previous.resize(width, outside);
        self.current.resize(width, outside);
        for column in 0..width {
            let value = u16::try_from(column.min(limit + 1)).unwrap_or(outside);
            self.previous_two[column] = value;
            self.previous[column] = value;
        }

        for row in 1..=left.len() {
            self.current[..width].fill(outside);
            if row <= limit {
                self.current[0] = row as u16;
            }
            let start = row.saturating_sub(limit).max(1);
            let end = row.saturating_add(limit).min(right.len());
            if start > end {
                return None;
            }
            let mut row_min = outside;
            for column in start..=end {
                let substitution = u16::from(left[row - 1] != right[column - 1]);
                let mut distance = self.current[column - 1]
                    .saturating_add(1)
                    .min(self.previous[column].saturating_add(1))
                    .min(self.previous[column - 1].saturating_add(substitution))
                    .min(outside);
                if row > 1
                    && column > 1
                    && left[row - 1] == right[column - 2]
                    && left[row - 2] == right[column - 1]
                {
                    distance = distance
                        .min(self.previous_two[column - 2].saturating_add(1))
                        .min(outside);
                }
                self.current[column] = distance;
                row_min = row_min.min(distance);
            }
            if usize::from(row_min) > limit {
                return None;
            }
            std::mem::swap(&mut self.previous_two, &mut self.previous);
            std::mem::swap(&mut self.previous, &mut self.current);
        }
        let distance = usize::from(self.previous[right.len()]);
        (distance <= limit).then_some(distance)
    }
}

#[cfg(test)]
fn bounded_osa_reference<T: Eq>(left: &[T], right: &[T], limit: usize) -> Option<usize> {
    if left.len().abs_diff(right.len()) > limit {
        return None;
    }
    let mut previous_two = (0..=right.len()).collect::<Vec<_>>();
    let mut previous = previous_two.clone();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_value) in left.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_min = current[0];
        for (right_index, right_value) in right.iter().enumerate() {
            let substitution = usize::from(left_value != right_value);
            let mut distance = (current[right_index] + 1)
                .min(previous[right_index + 1] + 1)
                .min(previous[right_index] + substitution);
            if left_index > 0
                && right_index > 0
                && left[left_index] == right[right_index - 1]
                && left[left_index - 1] == right[right_index]
            {
                distance = distance.min(previous_two[right_index - 1] + 1);
            }
            current[right_index + 1] = distance;
            row_min = row_min.min(distance);
        }
        if row_min > limit {
            return None;
        }
        std::mem::swap(&mut previous_two, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    (previous[right.len()] <= limit).then_some(previous[right.len()])
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
            inject(
                &mut candidates,
                index,
                &target,
                ctx.config.text_match.existing_path,
                is_dir,
                InjectedGroup::TextMatch,
            );
            let hits = candidates
                .into_iter()
                .map(|candidate| SearchHit {
                    name: candidate.name,
                    path: candidate.path,
                    is_directory: candidate.is_directory,
                    tier: candidate.tier.as_str(),
                    score: candidate.score,
                    contributions: candidate.contributions,
                    evidence: candidate.evidence,
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

    let prep = Prepared::new(index, trimmed, &query_lower, ctx.config);
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
        inject(
            &mut candidates,
            index,
            &target,
            ctx.config.alias.exact,
            true,
            InjectedGroup::Alias,
        );
    }
    inject_memory_candidates(&mut candidates, index, ctx);

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
            score: candidate.score,
            contributions: candidate.contributions,
            evidence: candidate.evidence,
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
        if let Some((mut score, path, text_evidence)) =
            score_entry(index, item, prep, ctx, &mut dir_masks, &mut scratch)
        {
            score += ctx.retrieval.boost(slot as u32, ctx.config);
            let (contributions, evidence) = score_contributions(
                score,
                slot as u32,
                &path,
                item.entry.parent,
                item.entry.is_directory,
                item.entry.tier,
                prep,
                ctx,
                index,
                text_evidence,
            );
            local.push(Candidate {
                score,
                contributions,
                evidence,
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
        if let Some((mut score, path, text_evidence)) =
            score_entry(index, item, prep, ctx, &mut dir_masks, &mut scratch)
        {
            score += ctx.retrieval.boost(slot, ctx.config);
            let (contributions, evidence) = score_contributions(
                score,
                slot,
                &path,
                item.entry.parent,
                item.entry.is_directory,
                item.entry.tier,
                prep,
                ctx,
                index,
                text_evidence,
            );
            local.push(Candidate {
                score,
                contributions,
                evidence,
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
) -> Option<(i64, String, TextMatchEvidence)> {
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
) -> Option<(i64, String, TextMatchEvidence)> {
    let (quality, corrected_match) = if let Some(quality) = quality_match(
        item.entry.name,
        item.name_filter,
        &prep.query_lower,
        prep.query_name_filter,
        scratch,
        &prep.text,
    ) {
        (quality, false)
    } else {
        let mut best: Option<MatchQuality> = None;
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
                &prep.text,
            ) {
                best = Some(best.map_or(quality, |current| current.max(quality)));
            }
        }
        (best?, true)
    };

    let text_evidence = TextMatchEvidence {
        feature: quality.feature,
        typo_edits: quality.typo_edits,
        layout_corrected: corrected_match,
        ..TextMatchEvidence::default()
    };
    let mut score = text_evidence.contribution(ctx.config);
    score -= tier_penalty(item.entry.tier, &ctx.config.penalties);

    let path = index.entry_path(item.entry);
    let path_str = path.to_string_lossy().into_owned();
    score += personal_boost(ctx, &path_str, item.entry.is_directory);
    if prep.known.contains(&path) {
        score += ctx.config.context.known_place;
    }
    Some((score, path_str, text_evidence))
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
) -> Option<(i64, String, TextMatchEvidence)> {
    // The direct token set is the flat needle prefix (offset 0); each corrected set follows at
    // its recorded offset, so a per-set token position maps to a flat bit index.
    let (mut text_evidence, corrected_match) = if let Some(band) =
        best_multi_interpretation(index, item, &prep.tokens, 0, prep, dir_masks, scratch)
    {
        (band, false)
    } else {
        let mut best: Option<TextMatchEvidence> = None;
        for (k, set) in prep.corrected_tokens.iter().enumerate() {
            // A corrected token of the other script never matches a same-script name or
            // path; `quality_match`/`contains_ci` reject that mismatch before any copy, so
            // this fallback stays cheap across a whole-index scan (e.g. a Cyrillic corrected
            // token over millions of ASCII names).
            let offset = prep.corrected_offsets[k];
            if let Some(band) =
                best_multi_interpretation(index, item, set, offset, prep, dir_masks, scratch)
            {
                best = Some(best.map_or(band.clone(), |current| {
                    if band.contribution(ctx.config) > current.contribution(ctx.config) {
                        band
                    } else {
                        current
                    }
                }));
            }
        }
        (best?, true)
    };

    text_evidence.layout_corrected = corrected_match;
    let mut score = text_evidence.contribution(ctx.config);
    score -= tier_penalty(item.entry.tier, &ctx.config.penalties);

    let path = index.entry_path(item.entry);
    let path_str = path.to_string_lossy().into_owned();
    score += personal_boost(ctx, &path_str, item.entry.is_directory);
    if prep.known.contains(&path) {
        score += ctx.config.context.known_place;
    }
    Some((score, path_str, text_evidence))
}

/// The strongest direct interpretation of a multi-token query. Ordinary matching allows
/// any token order; Path Interpretation additionally rewards an ordered ancestor chain
/// ending at the Item name. This is a score contribution, not a forced first position.
fn best_multi_interpretation(
    index: &IndexData,
    item: SearchItem<'_>,
    tokens: &[String],
    offset: usize,
    prep: &Prepared,
    dir_masks: &mut Option<DirMaskCache>,
    scratch: &mut String,
) -> Option<TextMatchEvidence> {
    let ordinary = match_token_set(index, item, tokens, offset, prep, dir_masks, scratch);
    let scoped = implicit_path_quality(index, item, tokens, offset, prep, scratch);
    match (ordinary, scoped) {
        (Some(left), Some(right)) => {
            if right.contribution_with_weights(&prep.text)
                > left.contribution_with_weights(&prep.text)
            {
                Some(right)
            } else {
                Some(left)
            }
        }
        (left, right) => left.or(right),
    }
}

fn implicit_path_quality(
    index: &IndexData,
    item: SearchItem<'_>,
    tokens: &[String],
    offset: usize,
    prep: &Prepared,
    scratch: &mut String,
) -> Option<TextMatchEvidence> {
    let (last, prefix) = tokens.split_last()?;
    if prefix.is_empty() {
        return None;
    }
    let last_filter = *prep
        .path_needle_name_filters
        .get(offset + tokens.len() - 1)?;
    let quality = quality_match(
        item.entry.name,
        item.name_filter,
        last,
        last_filter,
        scratch,
        &prep.text,
    )?;
    ancestors_match(index, item.entry.parent, prefix).then_some(TextMatchEvidence {
        feature: quality.feature,
        typo_edits: quality.typo_edits,
        path_scope: true,
        ..TextMatchEvidence::default()
    })
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
) -> Option<TextMatchEvidence> {
    let mut band: Option<MatchQuality> = None;
    let mut all_in_name = true;
    for (j, token) in tokens.iter().enumerate() {
        // `offset + j` is this token's index in `Prepared::path_needles`, i.e. its bit in a
        // dir mask.
        let quality = token_quality(index, item, token, offset + j, prep, dir_masks, scratch)?;
        band = Some(band.map_or(quality.quality, |current| current.min(quality.quality)));
        all_in_name &= quality.in_name;
    }
    let band = band?;
    Some(TextMatchEvidence {
        feature: band.feature,
        typo_edits: band.typo_edits,
        all_tokens_in_name: all_in_name,
        ..TextMatchEvidence::default()
    })
}

struct TokenQuality {
    quality: MatchQuality,
    in_name: bool,
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
) -> Option<TokenQuality> {
    if let Some(quality) = quality_match(
        item.entry.name,
        item.name_filter,
        token,
        prep.path_needle_name_filters[flat_index],
        scratch,
        &prep.text,
    ) {
        return Some(TokenQuality {
            quality,
            in_name: true,
        });
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
    in_path.then_some(TokenQuality {
        quality: MatchQuality::literal(prep.text.path_component, TextMatchFeature::PathComponent),
        in_name: false,
    })
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
) -> Option<(i64, String, TextMatchEvidence)> {
    let (last, prefix) = prep.segments.split_last()?;
    let last_filter = *prep.segment_name_filters.last()?;
    let quality = quality_match(
        item.entry.name,
        item.name_filter,
        last,
        last_filter,
        scratch,
        &prep.text,
    )?;

    let scoped = !prefix.is_empty() && ancestors_match(index, item.entry.parent, prefix);
    let text_evidence = TextMatchEvidence {
        feature: quality.feature,
        typo_edits: quality.typo_edits,
        path_scope: scoped,
        ..TextMatchEvidence::default()
    };
    let mut score = text_evidence.contribution(ctx.config);

    // Junk stays penalized even under a typed prefix; only the hidden penalty is waived
    // for a scoped match (guarantee c: "no hidden penalty").
    match item.entry.tier {
        Tier::Junk => score -= tier_penalty(Tier::Junk, &ctx.config.penalties),
        Tier::Hidden if !scoped => score -= tier_penalty(Tier::Hidden, &ctx.config.penalties),
        _ => {}
    }

    let path = index.entry_path(item.entry);
    let path_str = path.to_string_lossy().into_owned();
    score += personal_boost(ctx, &path_str, item.entry.is_directory);
    if prep.known.contains(&path) {
        score += ctx.config.context.known_place;
    }
    Some((score, path_str, text_evidence))
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MatchQuality {
    score: i64,
    feature: TextMatchFeature,
    typo_edits: u8,
}

impl MatchQuality {
    fn literal(score: i64, feature: TextMatchFeature) -> Self {
        Self {
            score,
            feature,
            typo_edits: 0,
        }
    }

    fn cap_as_path(self, weights: &TextMatchWeights) -> Self {
        if self.score <= weights.path_component {
            self
        } else {
            Self::literal(weights.path_component, TextMatchFeature::PathComponent)
        }
    }
}

fn quality_match(
    name: &str,
    item_filter: u64,
    needle: &str,
    needle_filter: u64,
    scratch: &mut String,
    weights: &TextMatchWeights,
) -> Option<MatchQuality> {
    if !filter_contains(item_filter, needle_filter) {
        return None;
    }
    if name.is_ascii() && needle.is_ascii() {
        return quality_ascii(name.as_bytes(), needle.as_bytes(), weights);
    }
    // A non-ASCII needle (e.g. a layout-corrected Cyrillic token) can never occur in an
    // ASCII name, so reject before copying the name into `scratch`. This is what keeps the
    // corrected-token fallback of a multi-token scan cheap across a whole-index scan of
    // ASCII names (the same short-circuit `score_plain` applies inline before its loop).
    if !needle.is_ascii() && name.is_ascii() {
        return None;
    }
    lower_into(name, scratch);
    quality_of(scratch, needle, weights)
}

/// Match quality of `needle` against an already-lowercased `haystack` (Unicode path).
fn quality_of(haystack: &str, needle: &str, weights: &TextMatchWeights) -> Option<MatchQuality> {
    if haystack == needle {
        Some(MatchQuality::literal(
            weights.exact_name,
            TextMatchFeature::ExactName,
        ))
    } else if haystack.starts_with(needle) {
        Some(MatchQuality::literal(
            weights.prefix_name,
            TextMatchFeature::PrefixName,
        ))
    } else if haystack.contains(needle) {
        Some(MatchQuality::literal(
            weights.substring_name,
            TextMatchFeature::SubstringName,
        ))
    } else {
        None
    }
}

/// Case-insensitive ASCII match quality of `needle` (lowercase) against `name` bytes.
fn quality_ascii(name: &[u8], needle: &[u8], weights: &TextMatchWeights) -> Option<MatchQuality> {
    if needle.is_empty() || name.len() < needle.len() {
        return None;
    }
    if name.len() == needle.len() && ascii_ci_eq(name, needle) {
        return Some(MatchQuality::literal(
            weights.exact_name,
            TextMatchFeature::ExactName,
        ));
    }
    if ascii_ci_eq(&name[..needle.len()], needle) {
        return Some(MatchQuality::literal(
            weights.prefix_name,
            TextMatchFeature::PrefixName,
        ));
    }
    if ascii_contains_ci(name, needle) {
        return Some(MatchQuality::literal(
            weights.substring_name,
            TextMatchFeature::SubstringName,
        ));
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

fn tier_penalty(tier: Tier, weights: &PenaltyWeights) -> i64 {
    let penalty = match tier {
        Tier::Normal => 0,
        Tier::Hidden => weights.hidden,
        Tier::Junk => weights.junk,
    };
    penalty.min(weights.total_cap)
}

fn personal_boost(ctx: &RankContext, path: &str, is_directory: bool) -> i64 {
    let visit = ctx
        .journal
        .strength_milli(path, &ctx.config.general_usage)
        .saturating_mul(ctx.config.general_usage.visit_max)
        / 1_000;
    let learned = ctx.memory.boost(path, ctx.config);
    let kind = if is_directory {
        ctx.config.item_kind.directory
    } else {
        ctx.config.item_kind.file
    };
    visit.saturating_add(learned).saturating_add(kind)
}

#[allow(clippy::too_many_arguments)]
fn score_contributions(
    total: i64,
    slot: u32,
    path: &str,
    parent: DirId,
    is_directory: bool,
    tier: Tier,
    prep: &Prepared,
    ctx: &RankContext,
    index: &IndexData,
    text_match: TextMatchEvidence,
) -> (ScoreContributions, ScoreEvidence) {
    let (search_memory_milli, learned_usage_milli) = ctx.memory.strengths_milli(path);
    let (search_memory, learned_usage) = ctx.memory.contributions(path, ctx.config);
    let visit_milli = ctx.journal.strength_milli(path, &ctx.config.general_usage);
    let visit = visit_milli.saturating_mul(ctx.config.general_usage.visit_max) / 1_000;
    let (retrieval_context, recents) = ctx.retrieval.contributions(slot, ctx.config);
    let is_known_place = prep.known.contains(Path::new(path));
    let known = i64::from(is_known_place).saturating_mul(ctx.config.context.known_place);
    let context = retrieval_context.saturating_add(known);
    let general_usage = learned_usage.saturating_add(visit).saturating_add(recents);
    let item_kind = if is_directory {
        ctx.config.item_kind.directory
    } else {
        ctx.config.item_kind.file
    };
    let scoped_hidden = tier == Tier::Hidden
        && prep.path_shaped
        && prep.segments.split_last().is_some_and(|(_, prefix)| {
            !prefix.is_empty() && ancestors_match(index, parent, prefix)
        });
    let penalties = if scoped_hidden {
        0
    } else {
        tier_penalty(tier, &ctx.config.penalties)
    };
    let contributions = ScoreContributions {
        text_match: text_match.contribution(ctx.config),
        search_memory,
        general_usage,
        context,
        alias: 0,
        item_kind,
        penalties,
    };
    debug_assert_eq!(contributions.total(), total);
    let evidence = ScoreEvidence {
        text_match,
        search_memory_milli,
        learned_usage_milli,
        visit_milli,
        current_location: ctx
            .retrieval
            .contains(slot, RetrievalSource::CurrentLocation),
        pinned_anchor: ctx.retrieval.contains(slot, RetrievalSource::PinnedAnchor),
        recents: ctx.retrieval.contains(slot, RetrievalSource::Recents),
        known_place: is_known_place,
        alias: false,
        item_kind_applied: true,
        is_directory,
        tier: tier.as_str().to_owned(),
        tier_penalty_waived: scoped_hidden,
    };
    (contributions, evidence)
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
pub(crate) fn layout_variants(query_lower: &str) -> Vec<String> {
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
#[derive(Clone, Copy)]
enum InjectedGroup {
    TextMatch,
    Alias,
}

fn inject(
    candidates: &mut Vec<Candidate>,
    index: &IndexData,
    target: &Path,
    score: i64,
    fallback_is_dir: bool,
    group: InjectedGroup,
) {
    let (is_directory, tier) = find_entry(index, target).unwrap_or((fallback_is_dir, Tier::Normal));
    let contributions = match group {
        InjectedGroup::TextMatch => ScoreContributions {
            text_match: score,
            ..ScoreContributions::default()
        },
        InjectedGroup::Alias => ScoreContributions {
            alias: score,
            ..ScoreContributions::default()
        },
    };
    let evidence = ScoreEvidence {
        text_match: if matches!(group, InjectedGroup::TextMatch) {
            TextMatchEvidence {
                feature: TextMatchFeature::ExistingPath,
                ..TextMatchEvidence::default()
            }
        } else {
            TextMatchEvidence::default()
        },
        alias: matches!(group, InjectedGroup::Alias),
        is_directory,
        tier: tier.as_str().to_owned(),
        tier_penalty_waived: true,
        ..ScoreEvidence::default()
    };
    let path_str = target.to_string_lossy().into_owned();
    if let Some(existing) = candidates.iter_mut().find(|c| c.path == path_str) {
        if score > existing.score {
            existing.score = score;
            existing.contributions = contributions;
            existing.evidence = evidence;
        }
        return;
    }
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_str.clone());
    candidates.push(Candidate {
        score,
        contributions,
        evidence,
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

/// Merge query-relevant learned Items after retrieval. Existing textual candidates retain
/// their match score plus the memory contribution; an Item found only through Search Memory
/// enters with that contribution as its whole score. The learned store is bounded, so this
/// never turns the global Name Index scan into a path walk.
fn inject_memory_candidates(candidates: &mut Vec<Candidate>, index: &IndexData, ctx: &RankContext) {
    for path in ctx.memory.paths() {
        let (search_memory, general_usage) = ctx.memory.contributions(path, ctx.config);
        if search_memory <= 0 && general_usage <= 0 {
            continue;
        }
        if candidates.iter().any(|candidate| candidate.path == path) {
            // Textual candidates already received this contribution inside `score_entry`.
            // Do not add it twice.
            continue;
        }
        let target = Path::new(path);
        let Some((is_directory, tier)) = find_entry(index, target) else {
            // Dormant deleted/unmounted evidence never creates a ghost result.
            continue;
        };
        let item_kind = if is_directory {
            ctx.config.item_kind.directory
        } else {
            ctx.config.item_kind.file
        };
        let contributions = ScoreContributions {
            text_match: 0,
            search_memory,
            general_usage,
            context: 0,
            alias: 0,
            item_kind,
            penalties: tier_penalty(tier, &ctx.config.penalties),
        };
        let (search_memory_milli, learned_usage_milli) = ctx.memory.strengths_milli(path);
        let evidence = ScoreEvidence {
            search_memory_milli,
            learned_usage_milli,
            item_kind_applied: true,
            is_directory,
            tier: tier.as_str().to_owned(),
            ..ScoreEvidence::default()
        };
        let score = contributions.total();
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned());
        candidates.push(Candidate {
            score,
            contributions,
            evidence,
            path: path.to_owned(),
            name,
            is_directory,
            tier,
            slot: u32::MAX,
        });
    }
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
                &RankerConfig::default().text_match,
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
            retrieval: RetrievalSignals::default(),
            memory: MemoryEvidence::empty(),
            config: RankContext::empty().config,
        };
        let boosted = search(&index, &ctx, "notes", 50);
        assert_eq!(boosted[0].path, "/home/tester/sub/notes"); // visited path lifted
    }

    #[test]
    fn search_memory_can_retrieve_an_item_without_a_text_match() {
        let mut index = index();
        index.add_file(0, "methodology-notes.md", Tier::Normal);
        index.add_file(0, "unrelated.txt", Tier::Normal);
        let memory = MemoryEvidence::from_scores(&[("/home/tester/methodology-notes.md", 1_000)]);
        let ctx = RankContext {
            journal: &Aggregate::default(),
            aliases: &AliasDictionary::empty(),
            retrieval: RetrievalSignals::default(),
            memory: &memory,
            config: RankContext::empty().config,
        };

        let hits = search(&index, &ctx, "project alpha", 50);
        assert_eq!(paths(&hits), vec!["/home/tester/methodology-notes.md"]);
        assert_eq!(hits[0].score, hits[0].contributions.total());
        assert!(hits[0].contributions.search_memory > 0);
        assert_eq!(hits[0].contributions.text_match, 0);
    }

    #[test]
    fn memory_only_result_keeps_its_tier_penalty() {
        let mut index = index();
        index.add_file(0, ".private", Tier::Hidden);
        let memory = MemoryEvidence::from_scores(&[("/home/tester/.private", 1_000)]);
        let ctx = RankContext {
            journal: &Aggregate::default(),
            aliases: &AliasDictionary::empty(),
            retrieval: RetrievalSignals::default(),
            memory: &memory,
            config: RankContext::empty().config,
        };

        let hits = search(&index, &ctx, "unrelated query", 50);
        assert_eq!(hits[0].path, "/home/tester/.private");
        assert_eq!(hits[0].contributions.penalties, ctx.config.penalties.hidden);
        assert_eq!(hits[0].score, hits[0].contributions.total());
    }

    #[test]
    fn every_ranked_score_equals_its_named_contributions() {
        let mut index = index();
        let docs = index.add_dir(0, "Documents", Tier::Normal, 0);
        index.add_file(docs, "methodology-report.md", Tier::Normal);
        index.add_file(0, "methodology.txt", Tier::Hidden);
        let hits = run(&index, "methodology");
        assert!(!hits.is_empty());
        for hit in hits {
            assert_eq!(hit.score, hit.contributions.total(), "{}", hit.path);
        }
    }

    #[test]
    fn recents_source_lifts_within_quality_without_crossing_a_quality_band() {
        let mut index = index();
        index.add_file(0, "report", Tier::Normal);
        let sub = index.add_dir(0, "sub", Tier::Normal, 0);
        let recent_slot = index.slot_len() as u32;
        index.add_file(sub, "report", Tier::Normal);
        let weaker_slot = index.slot_len() as u32;
        index.add_file(0, "report-draft", Tier::Normal);

        let mut retrieval = RetrievalSignals::default();
        retrieval.insert(recent_slot, RetrievalSource::Recents);
        retrieval.insert(weaker_slot, RetrievalSource::Recents);
        let ctx = RankContext {
            journal: &Aggregate::default(),
            aliases: &AliasDictionary::empty(),
            retrieval,
            memory: MemoryEvidence::empty(),
            config: RankContext::empty().config,
        };
        let hits = search(&index, &ctx, "report", 50);

        assert_eq!(hits[0].path, "/home/tester/sub/report");
        assert_eq!(hits[1].path, "/home/tester/report");
        assert_eq!(hits[2].path, "/home/tester/report-draft");
    }

    #[test]
    fn current_location_can_lift_a_direct_child_prefix_over_global_exact_names() {
        let mut index = index();
        index.add_file(0, "skills", Tier::Normal);
        let current = index.add_dir(0, "current", Tier::Normal, 0);
        let local_slot = index.slot_len() as u32;
        index.add_file(current, "skills-drafts", Tier::Normal);

        let mut retrieval = RetrievalSignals::default();
        retrieval.insert(local_slot, RetrievalSource::CurrentLocation);
        let ctx = RankContext {
            journal: &Aggregate::default(),
            aliases: &AliasDictionary::empty(),
            retrieval,
            memory: MemoryEvidence::empty(),
            config: RankContext::empty().config,
        };

        let hits = search(&index, &ctx, "skills", 50);
        assert_eq!(hits[0].path, "/home/tester/current/skills-drafts");
        assert_eq!(hits[1].path, "/home/tester/skills");
    }

    #[test]
    fn ranker_config_changes_order_without_changing_retrieval() {
        let mut index = index();
        index.add_file(0, "skills", Tier::Normal);
        let current = index.add_dir(0, "current", Tier::Normal, 0);
        let local_slot = index.slot_len() as u32;
        index.add_file(current, "skills-drafts", Tier::Normal);
        let mut retrieval = RetrievalSignals::default();
        retrieval.insert(local_slot, RetrievalSource::CurrentLocation);

        let mut config = RankerConfig::default();
        config.context.current_location = 0;
        let without_boost = RankContext {
            journal: &Aggregate::default(),
            aliases: &AliasDictionary::empty(),
            retrieval: retrieval.clone(),
            memory: MemoryEvidence::empty(),
            config: &config,
        };
        assert_eq!(
            search(&index, &without_boost, "skills", 50)[0].path,
            "/home/tester/skills"
        );

        config.context.current_location = 1_500_000;
        let with_boost = RankContext {
            journal: &Aggregate::default(),
            aliases: &AliasDictionary::empty(),
            retrieval,
            memory: MemoryEvidence::empty(),
            config: &config,
        };
        assert_eq!(
            search(&index, &with_boost, "skills", 50)[0].path,
            "/home/tester/current/skills-drafts"
        );
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
            retrieval: RetrievalSignals::default(),
            memory: MemoryEvidence::empty(),
            config: RankContext::empty().config,
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
    fn ordered_tokens_strongly_boost_an_implicit_path() {
        let mut index = index();
        let work = index.add_dir(0, "work", Tier::Normal, 0);
        index.add_dir(work, "wip", Tier::Normal, 0);
        index.add_file(0, "archive-users-work-wip-session.md", Tier::Normal);

        let hits = run(&index, "work wip");
        assert_eq!(hits[0].path, "/home/tester/work/wip");
    }

    #[test]
    fn all_name_tokens_beat_a_name_and_ancestor_path_interpretation() {
        let mut index = index();
        let status = index.add_dir(0, "status-all", Tier::Normal, 0);
        index.add_file(status, "report-data.ts", Tier::Normal);
        let downloads = index.add_dir(0, "Downloads", Tier::Normal, 0);
        index.add_file(downloads, "status-report_2026.xlsx", Tier::Normal);

        let hits = run(&index, "status report");
        assert_eq!(
            hits[0].path,
            "/home/tester/Downloads/status-report_2026.xlsx"
        );
        assert_eq!(hits[1].path, "/home/tester/status-all/report-data.ts");
    }

    #[test]
    fn ordered_tail_literal_overlay_scan_keeps_only_literal_name_candidates() {
        let mut index = index();
        let vault = index.add_dir(0, "vault", Tier::Normal, 0);
        let target_slot = index.slot_len() as u32;
        index.add_file(vault, "methodology.md", Tier::Normal);
        index.add_file(0, "vault-notes.md", Tier::Normal);
        index.add_file(0, "unrelated.txt", Tier::Normal);

        let selected = ordered_tail_literal_slots(&index, "vault methodology", None);
        assert!(!selected.aborted);
        assert_eq!(selected.slots, vec![target_slot]);

        let generation = AtomicU64::new(2);
        let cancelled = Cancel::new(&generation, 1);
        assert!(ordered_tail_literal_slots(&index, "vault methodology", Some(&cancelled)).aborted);
    }

    #[test]
    fn fuzzy_streaming_partials_converge_to_final_order() {
        let mut index = index();
        let mut slots = Vec::new();
        for number in 0..=PAR_THRESHOLD {
            let name = if number.is_multiple_of(10_000) {
                format!("methodology-{number}.md")
            } else {
                format!("unrelated-{number}.txt")
            };
            let slot = index.slot_len() as u32;
            index.add_file(0, &name, Tier::Normal);
            slots.push(slot);
        }
        let mut partials = Vec::new();
        let outcome = run_fuzzy_streaming(
            &index,
            &RankContext::empty(),
            "methodolgy",
            50,
            &slots,
            None,
            false,
            &mut |hits, _| partials.push(hits),
        );

        assert!(!partials.is_empty());
        assert_eq!(partials.last(), Some(&outcome.hits));
        assert_eq!(outcome.hits.len(), 6);
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

    #[test]
    fn fuzzy_candidates_cover_typo_layout_and_transposition() {
        let mut index = index();
        index.add_file(0, "методология.md", Tier::Normal);
        index.add_file(0, "methodology.md", Tier::Normal);
        let slots = [0, 1];
        let ctx = RankContext::empty();

        for query in ["метолология", "methodolgy", "methdoology", "ьуерщвщдпн"]
        {
            let outcome = super::run_fuzzy(&index, &ctx, query, 50, &slots, None, false);
            assert!(
                outcome
                    .hits
                    .iter()
                    .any(|hit| hit.name == "методология.md" || hit.name == "methodology.md"),
                "no corrected result for {query}"
            );
        }
    }

    #[test]
    fn banded_osa_matches_full_reference_exhaustively() {
        fn strings(alphabet: &[char], max_len: usize) -> Vec<String> {
            fn extend(all: &mut Vec<String>, prefix: &mut String, alphabet: &[char], left: usize) {
                all.push(prefix.clone());
                if left == 0 {
                    return;
                }
                for character in alphabet {
                    prefix.push(*character);
                    extend(all, prefix, alphabet, left - 1);
                    prefix.pop();
                }
            }
            let mut all = Vec::new();
            extend(&mut all, &mut String::new(), alphabet, max_len);
            all
        }

        let corpus = strings(&['a', 'b', 'ж'], 4);
        let mut scratch = EditScratch::default();
        for left in &corpus {
            for right in &corpus {
                let left_chars = left.chars().collect::<Vec<_>>();
                let right_chars = right.chars().collect::<Vec<_>>();
                for limit in 0..=3 {
                    assert_eq!(
                        scratch.distance(left, right, limit),
                        bounded_osa_reference(&left_chars, &right_chars, limit),
                        "left={left:?}, right={right:?}, limit={limit}"
                    );
                }
            }
        }
    }
}
