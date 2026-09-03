mod index;
mod matcher;
mod qgram;

pub(crate) use index as name_index;

mod qos {
    pub fn set_user_initiated_qos() {}
}

use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering as AtomicOrdering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use index::{model::Tier, LiveIndex};
use matcher::{
    best_text_evidence, could_match_name, normalize, AncestorCache, PreparedQuery, TextEvidence,
};
use qgram::{BuildReport as QGramBuildReport, QGramIndex};
use serde::Serialize;
use serde_json::Value;

const GLOBAL_THRESHOLD: usize = 5;
const RESULT_LIMIT: usize = 50;
const SHARD_TOP_K: usize = 256;
const MAX_SHARDS: usize = 8;
const CANCEL_STRIDE: usize = 4096;

#[derive(Clone)]
struct Case {
    id: String,
    category: String,
    query: String,
    target: PathBuf,
}

struct Config {
    index: PathBuf,
    root: PathBuf,
    current: PathBuf,
    recents: PathBuf,
    journal: PathBuf,
    pinned: Option<PathBuf>,
    cases: PathBuf,
    samples: usize,
    qgram_index: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum GlobalRetrieval<'a> {
    Mmap,
    QGram(&'a QGramIndex),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    slot: u32,
    path: String,
    name: String,
    is_directory: bool,
    tier: &'static str,
    text: TextEvidence,
    context_contribution: i64,
    general_usage_contribution: i64,
    penalty: i64,
    score: i64,
    #[serde(skip)]
    sort_name: String,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.slot == other.slot && self.score == other.score
    }
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .cmp(&other.score)
            .then_with(|| other.sort_name.cmp(&self.sort_name))
            .then_with(|| other.path.cmp(&self.path))
            .then_with(|| other.slot.cmp(&self.slot))
    }
}

#[derive(Clone)]
struct TopK {
    limit: usize,
    heap: BinaryHeap<Reverse<Candidate>>,
    slots: HashSet<u32>,
}

impl TopK {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::new(),
            slots: HashSet::new(),
        }
    }

    fn push(&mut self, candidate: Candidate) {
        if self.slots.contains(&candidate.slot) {
            return;
        }
        if self.heap.len() < self.limit {
            self.slots.insert(candidate.slot);
            self.heap.push(Reverse(candidate));
            return;
        }
        let should_replace = self.heap.peek().is_some_and(|worst| candidate > worst.0);
        if should_replace {
            if let Some(removed) = self.heap.pop() {
                self.slots.remove(&removed.0.slot);
            }
            self.slots.insert(candidate.slot);
            self.heap.push(Reverse(candidate));
        }
    }

    fn extend(&mut self, candidates: impl IntoIterator<Item = Candidate>) {
        for candidate in candidates {
            self.push(candidate);
        }
    }

    fn sorted(&self) -> Vec<Candidate> {
        let mut candidates = self
            .heap
            .iter()
            .map(|candidate| candidate.0.clone())
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.cmp(left));
        candidates
    }

    fn identity_top(&self, count: usize) -> Vec<u32> {
        self.sorted()
            .into_iter()
            .take(count)
            .map(|candidate| candidate.slot)
            .collect()
    }
}

#[derive(Default)]
struct ScanStats {
    scanned: usize,
    prefilter_passed: usize,
    matched: usize,
    aborted: bool,
}

struct ScanPart {
    top: TopK,
    stats: ScanStats,
}

#[derive(Clone, Copy)]
struct RankingContext<'a> {
    current: &'a HashSet<u32>,
    historical: &'a HashSet<u32>,
}

#[derive(Clone, Copy)]
struct CancelCheck<'a> {
    generation: &'a AtomicU64,
    mine: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StageSample {
    working_set_ms: f64,
    exact_global_ms: Option<f64>,
    candidate_retrieval_ms: Option<f64>,
    first_global_wave_ms: Option<f64>,
    stable_top_ten_ms: f64,
    final_top_fifty_ms: f64,
    global_ms: Option<f64>,
    rank_ms: f64,
    reorder_waves: usize,
    exact_scanned: usize,
    global_candidates: usize,
    posting_visits: usize,
    scanned: usize,
    prefilter_passed: usize,
    matched: usize,
    aborted: bool,
}

struct RunOutcome {
    sample: StageSample,
    hits: Vec<Candidate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Distribution {
    p50: f64,
    p90: f64,
    p95: f64,
    max: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseReport {
    id: String,
    category: String,
    query: String,
    significant_characters: usize,
    target: String,
    target_in_working_set: bool,
    target_rank: Option<usize>,
    working_set: Distribution,
    exact_global: Option<Distribution>,
    candidate_retrieval: Option<Distribution>,
    first_global_wave: Option<Distribution>,
    stable_top_ten: Distribution,
    final_top_fifty: Distribution,
    global: Option<Distribution>,
    rank: Distribution,
    reorder_waves: Vec<usize>,
    exact_scanned: usize,
    global_candidates: usize,
    posting_visits: usize,
    scanned: usize,
    prefilter_passed: usize,
    matched: usize,
    top_hits: Vec<Candidate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CancellationReport {
    retrieval: &'static str,
    query: String,
    cancel_after_ms: f64,
    observed_after_ms: f64,
    work_units_before_abort: usize,
    aborted: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QGramReport {
    path: String,
    built: bool,
    build_ms: Option<f64>,
    bytes: u64,
    postings: u64,
    buckets: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    index_path: String,
    index_bytes: u64,
    index_items: usize,
    index_directories: usize,
    index_load_ms: f64,
    current_location: String,
    current_location_items: usize,
    historical_items: usize,
    working_set_items: usize,
    global_threshold: usize,
    shards: usize,
    samples: usize,
    global_retrieval: &'static str,
    qgram: Option<QGramReport>,
    cases: Vec<CaseReport>,
    cancellation: CancellationReport,
    limitations: Vec<&'static str>,
}

fn main() {
    let config = parse_args();
    let index_bytes = fs::metadata(&config.index).expect("stat Name Index").len();
    let load_started = Instant::now();
    let index = LiveIndex::load(&config.index, &config.root);
    let index_load_ms = elapsed_ms(load_started);
    let (qgram, qgram_report) = config.qgram_index.as_ref().map_or((None, None), |path| {
        let (qgram, build) = QGramIndex::open_or_build(path, &index);
        (Some(qgram), Some(qgram_report(path, build)))
    });
    let retrieval = qgram
        .as_ref()
        .map_or(GlobalRetrieval::Mmap, GlobalRetrieval::QGram);

    let current_slots = index.direct_slots(&config.current);
    let mut historical_paths = read_recents(&config.recents);
    historical_paths.extend(read_journal(&config.journal));
    if let Some(path) = &config.pinned {
        historical_paths.extend(read_pinned(path));
    }
    let historical_slots = index.slots_for_paths(historical_paths.iter().map(PathBuf::as_path));
    let current = current_slots.iter().copied().collect::<HashSet<_>>();
    let historical = historical_slots.iter().copied().collect::<HashSet<_>>();
    let working_slots = current_slots
        .iter()
        .chain(&historical_slots)
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let working = working_slots.iter().copied().collect::<HashSet<_>>();

    let cases = read_cases(&config.cases)
        .into_iter()
        .map(|case| {
            measure_case(
                &index,
                case,
                &working_slots,
                &working,
                &current,
                &historical,
                config.samples,
                retrieval,
            )
        })
        .collect::<Vec<_>>();
    let cancellation = measure_cancellation(&index, &current, &historical, retrieval);
    let shards = shard_count(index.slot_count());

    let report = Report {
        index_path: config.index.to_string_lossy().into_owned(),
        index_bytes,
        index_items: index.item_count(),
        index_directories: index.directory_count(),
        index_load_ms,
        current_location: config.current.to_string_lossy().into_owned(),
        current_location_items: current_slots.len(),
        historical_items: historical_slots.len(),
        working_set_items: working_slots.len(),
        global_threshold: GLOBAL_THRESHOLD,
        shards,
        samples: config.samples,
        global_retrieval: match retrieval {
            GlobalRetrieval::Mmap => "mmap_full_scan",
            GlobalRetrieval::QGram(_) => "qgram_candidates",
        },
        qgram: qgram_report,
        cases,
        cancellation,
        limitations: vec![
            "Rust core only: IPC and rendered UI are not measured",
            "Search Memory has no real learned-state distribution yet",
            "mmap baseline emits one wave per completed scan shard",
            "ordinary multi-token candidates require at least one name-token match",
            "q-gram retrieval is a measured prototype and does not yet provide a formal no-false-negative proof",
        ],
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize report")
    );
}

fn qgram_report(path: &Path, build: QGramBuildReport) -> QGramReport {
    QGramReport {
        path: path.to_string_lossy().into_owned(),
        built: build.built,
        build_ms: build.build_ms,
        bytes: build.bytes,
        postings: build.postings,
        buckets: build.buckets,
    }
}

#[allow(clippy::too_many_arguments)]
fn measure_case(
    index: &LiveIndex,
    case: Case,
    working_slots: &[u32],
    working: &HashSet<u32>,
    current: &HashSet<u32>,
    historical: &HashSet<u32>,
    samples: usize,
    retrieval: GlobalRetrieval<'_>,
) -> CaseReport {
    let prepared = PreparedQuery::new(&case.query);
    let target_slot = index.resolve_item_slot(&case.target);
    let target_in_working_set = target_slot.is_some_and(|slot| working.contains(&slot));
    let generation = AtomicU64::new(1);
    let _ = run_query(
        index,
        &prepared,
        working_slots,
        current,
        historical,
        &generation,
        1,
        retrieval,
    );
    let mut outcomes = Vec::with_capacity(samples);
    for _ in 0..samples {
        outcomes.push(run_query(
            index,
            &prepared,
            working_slots,
            current,
            historical,
            &generation,
            1,
            retrieval,
        ));
    }
    let last = outcomes.last().expect("at least one sample");
    let target_rank = target_slot.and_then(|slot| {
        last.hits
            .iter()
            .position(|candidate| candidate.slot == slot)
            .map(|position| position + 1)
    });
    let samples_only = outcomes
        .iter()
        .map(|outcome| &outcome.sample)
        .collect::<Vec<_>>();
    CaseReport {
        id: case.id,
        category: case.category,
        query: case.query,
        significant_characters: prepared.significant_characters,
        target: case.target.to_string_lossy().into_owned(),
        target_in_working_set,
        target_rank,
        working_set: distribution(samples_only.iter().map(|sample| sample.working_set_ms)),
        exact_global: optional_distribution(
            samples_only
                .iter()
                .filter_map(|sample| sample.exact_global_ms),
        ),
        candidate_retrieval: optional_distribution(
            samples_only
                .iter()
                .filter_map(|sample| sample.candidate_retrieval_ms),
        ),
        first_global_wave: optional_distribution(
            samples_only
                .iter()
                .filter_map(|sample| sample.first_global_wave_ms),
        ),
        stable_top_ten: distribution(samples_only.iter().map(|sample| sample.stable_top_ten_ms)),
        final_top_fifty: distribution(samples_only.iter().map(|sample| sample.final_top_fifty_ms)),
        global: optional_distribution(samples_only.iter().filter_map(|sample| sample.global_ms)),
        rank: distribution(samples_only.iter().map(|sample| sample.rank_ms)),
        reorder_waves: samples_only
            .iter()
            .map(|sample| sample.reorder_waves)
            .collect(),
        exact_scanned: last.sample.exact_scanned,
        global_candidates: last.sample.global_candidates,
        posting_visits: last.sample.posting_visits,
        scanned: last.sample.scanned,
        prefilter_passed: last.sample.prefilter_passed,
        matched: last.sample.matched,
        top_hits: last.hits.iter().take(5).cloned().collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_query(
    index: &LiveIndex,
    prepared: &PreparedQuery,
    working_slots: &[u32],
    current: &HashSet<u32>,
    historical: &HashSet<u32>,
    generation: &AtomicU64,
    mine: u64,
    retrieval: GlobalRetrieval<'_>,
) -> RunOutcome {
    let query_started = Instant::now();
    let ranking = RankingContext {
        current,
        historical,
    };
    let cancel = CancelCheck { generation, mine };
    let working_part = scan_slots(
        index,
        prepared,
        working_slots.iter().copied(),
        ranking,
        true,
        cancel,
    );
    let working_set_ms = elapsed_ms(query_started);
    let mut combined = TopK::new(SHARD_TOP_K);
    combined.extend(working_part.top.sorted());
    let initial_top = combined.identity_top(10);

    let mut first_global_wave_ms = None;
    let mut exact_global_ms = None;
    let mut candidate_retrieval_ms = None;
    let mut exact_scanned = 0;
    let mut global_candidates = 0;
    let mut posting_visits = 0;
    let mut stable_top_ten_ms = working_set_ms;
    let mut global_ms = None;
    let mut reorder_waves = 0;
    let mut stats = working_part.stats;
    if prepared.significant_characters >= GLOBAL_THRESHOLD && !stats.aborted {
        let global_started = Instant::now();
        let exact_started = Instant::now();
        let exact = index.exact_search(&prepared.query, generation, mine);
        exact_global_ms = Some(elapsed_ms(exact_started));
        exact_scanned = exact.scanned;
        stats.aborted |= exact.aborted;
        let mut prior_top = initial_top;
        let exact_matches = merge_exact_hits(index, prepared, exact.paths, ranking, &mut combined);
        if exact_matches > 0 {
            first_global_wave_ms = Some(elapsed_ms(query_started));
            let next_top = combined.identity_top(10);
            if next_top != prior_top {
                reorder_waves += 1;
                stable_top_ten_ms = elapsed_ms(query_started);
                prior_top = next_top;
            }
        }

        if !stats.aborted {
            match retrieval {
                GlobalRetrieval::Mmap => {
                    global_candidates = index.slot_count();
                    let (sender, receiver) = mpsc::channel();
                    let ranges = shard_ranges(index.slot_count());
                    thread::scope(|scope| {
                        for (start, end) in ranges {
                            let sender = sender.clone();
                            scope.spawn(move || {
                                let part = scan_slots(
                                    index,
                                    prepared,
                                    (start..end).map(|slot| slot as u32),
                                    ranking,
                                    false,
                                    cancel,
                                );
                                let _ = sender.send(part);
                            });
                        }
                        drop(sender);
                        for part in receiver {
                            merge_scan_wave(
                                part,
                                query_started,
                                &mut first_global_wave_ms,
                                &mut stats,
                                &mut combined,
                                &mut prior_top,
                                &mut reorder_waves,
                                &mut stable_top_ten_ms,
                            );
                        }
                    });
                }
                GlobalRetrieval::QGram(qgram) => {
                    let retrieval_started = Instant::now();
                    let candidate_set = qgram.candidates(prepared, generation, mine);
                    candidate_retrieval_ms = Some(elapsed_ms(retrieval_started));
                    global_candidates = candidate_set.slots.len();
                    posting_visits = candidate_set.posting_visits;
                    stats.aborted |= candidate_set.aborted;
                    let (sender, receiver) = mpsc::channel();
                    let chunks = candidate_chunks(&candidate_set.slots);
                    thread::scope(|scope| {
                        for chunk in chunks {
                            let sender = sender.clone();
                            scope.spawn(move || {
                                let part = scan_slots(
                                    index,
                                    prepared,
                                    chunk.iter().copied(),
                                    ranking,
                                    false,
                                    cancel,
                                );
                                let _ = sender.send(part);
                            });
                        }
                        drop(sender);
                        for part in receiver {
                            merge_scan_wave(
                                part,
                                query_started,
                                &mut first_global_wave_ms,
                                &mut stats,
                                &mut combined,
                                &mut prior_top,
                                &mut reorder_waves,
                                &mut stable_top_ten_ms,
                            );
                        }
                    });
                }
            }
        }
        global_ms = Some(elapsed_ms(global_started));
    }

    let rank_started = Instant::now();
    let hits = combined
        .sorted()
        .into_iter()
        .take(RESULT_LIMIT)
        .collect::<Vec<_>>();
    let rank_ms = elapsed_ms(rank_started);
    let final_top_fifty_ms = elapsed_ms(query_started);
    RunOutcome {
        sample: StageSample {
            working_set_ms,
            exact_global_ms,
            candidate_retrieval_ms,
            first_global_wave_ms,
            stable_top_ten_ms,
            final_top_fifty_ms,
            global_ms,
            rank_ms,
            reorder_waves,
            exact_scanned,
            global_candidates,
            posting_visits,
            scanned: stats.scanned,
            prefilter_passed: stats.prefilter_passed,
            matched: stats.matched,
            aborted: stats.aborted,
        },
        hits,
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_scan_wave(
    part: ScanPart,
    query_started: Instant,
    first_global_wave_ms: &mut Option<f64>,
    stats: &mut ScanStats,
    combined: &mut TopK,
    prior_top: &mut Vec<u32>,
    reorder_waves: &mut usize,
    stable_top_ten_ms: &mut f64,
) {
    if first_global_wave_ms.is_none() {
        *first_global_wave_ms = Some(elapsed_ms(query_started));
    }
    stats.scanned += part.stats.scanned;
    stats.prefilter_passed += part.stats.prefilter_passed;
    stats.matched += part.stats.matched;
    stats.aborted |= part.stats.aborted;
    combined.extend(part.top.sorted());
    let next_top = combined.identity_top(10);
    if next_top != *prior_top {
        *reorder_waves += 1;
        *stable_top_ten_ms = elapsed_ms(query_started);
        *prior_top = next_top;
    }
}

fn merge_exact_hits(
    index: &LiveIndex,
    prepared: &PreparedQuery,
    paths: Vec<String>,
    ranking: RankingContext<'_>,
    combined: &mut TopK,
) -> usize {
    let mut merged = 0;
    let mut ancestors = AncestorCache::default();
    for path in paths {
        let Some(slot) = index.resolve_item_slot(Path::new(&path)) else {
            continue;
        };
        let Some(entry) = index.entry(slot) else {
            continue;
        };
        let Some(text) = best_text_evidence(index, entry, prepared, false, &mut ancestors) else {
            continue;
        };
        combined.push(rank_candidate(index, slot, entry, text, ranking));
        merged += 1;
    }
    merged
}

fn scan_slots(
    index: &LiveIndex,
    prepared: &PreparedQuery,
    slots: impl IntoIterator<Item = u32>,
    ranking: RankingContext<'_>,
    short_fuzzy: bool,
    cancel: CancelCheck<'_>,
) -> ScanPart {
    let mut top = TopK::new(SHARD_TOP_K);
    let mut stats = ScanStats::default();
    let mut ancestors = AncestorCache::default();
    for slot in slots {
        if stats.scanned % CANCEL_STRIDE == 0
            && cancel.generation.load(AtomicOrdering::Relaxed) != cancel.mine
        {
            stats.aborted = true;
            break;
        }
        stats.scanned += 1;
        let Some(entry) = index.entry(slot) else {
            continue;
        };
        if !could_match_name(entry, prepared, short_fuzzy) {
            continue;
        }
        stats.prefilter_passed += 1;
        let Some(text) = best_text_evidence(index, entry, prepared, short_fuzzy, &mut ancestors)
        else {
            continue;
        };
        stats.matched += 1;
        top.push(rank_candidate(index, slot, entry, text, ranking));
    }
    ScanPart { top, stats }
}

fn rank_candidate(
    index: &LiveIndex,
    slot: u32,
    entry: index::model::EntryRef<'_>,
    text: TextEvidence,
    ranking: RankingContext<'_>,
) -> Candidate {
    let context_contribution = if ranking.current.contains(&slot) {
        90
    } else {
        0
    };
    let general_usage_contribution = if ranking.historical.contains(&slot) {
        70
    } else {
        0
    };
    let penalty = match entry.tier {
        Tier::Normal => 0,
        Tier::Hidden => 80,
        Tier::Junk => 250,
    };
    let score = text.contribution + context_contribution + general_usage_contribution - penalty;
    let path = index.path(entry).to_string_lossy().into_owned();
    Candidate {
        slot,
        path,
        name: entry.name.to_owned(),
        is_directory: entry.is_directory,
        tier: entry.tier.as_str(),
        text,
        context_contribution,
        general_usage_contribution,
        penalty,
        score,
        sort_name: normalize(entry.name),
    }
}

fn measure_cancellation(
    index: &LiveIndex,
    current: &HashSet<u32>,
    historical: &HashSet<u32>,
    retrieval: GlobalRetrieval<'_>,
) -> CancellationReport {
    let query = "methodolgy";
    let prepared = PreparedQuery::new(query);
    let generation = AtomicU64::new(1);
    let started = Instant::now();
    let (outcome, cancelled_at) = thread::scope(|scope| {
        let handle = scope.spawn(|| {
            run_query(
                index,
                &prepared,
                &[],
                current,
                historical,
                &generation,
                1,
                retrieval,
            )
        });
        thread::sleep(Duration::from_millis(1));
        let cancelled_at = Instant::now();
        generation.store(2, AtomicOrdering::Relaxed);
        (handle.join().expect("join cancellation scan"), cancelled_at)
    });
    CancellationReport {
        retrieval: match retrieval {
            GlobalRetrieval::Mmap => "mmap_full_scan",
            GlobalRetrieval::QGram(_) => "qgram_candidates",
        },
        query: query.to_owned(),
        cancel_after_ms: cancelled_at.duration_since(started).as_secs_f64() * 1000.0,
        observed_after_ms: elapsed_ms(cancelled_at),
        work_units_before_abort: outcome.sample.exact_scanned
            + outcome.sample.posting_visits
            + outcome.sample.scanned,
        aborted: outcome.sample.aborted,
    }
}

fn shard_count(items: usize) -> usize {
    thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_SHARDS)
        .min(items.max(1))
}

fn shard_ranges(items: usize) -> Vec<(usize, usize)> {
    let shards = shard_count(items);
    let chunk = items.div_ceil(shards);
    (0..shards)
        .map(|shard| {
            let start = shard * chunk;
            (start, (start + chunk).min(items))
        })
        .filter(|(start, end)| start < end)
        .collect()
}

fn candidate_chunks(slots: &[u32]) -> Vec<&[u32]> {
    if slots.is_empty() {
        return Vec::new();
    }
    let chunk = slots.len().div_ceil(shard_count(slots.len()));
    slots.chunks(chunk).collect()
}

fn distribution(values: impl IntoIterator<Item = f64>) -> Distribution {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    assert!(!values.is_empty(), "distribution requires samples");
    Distribution {
        p50: percentile(&values, 0.50),
        p90: percentile(&values, 0.90),
        p95: percentile(&values, 0.95),
        max: *values.last().expect("non-empty distribution"),
    }
}

fn optional_distribution(values: impl IntoIterator<Item = f64>) -> Option<Distribution> {
    let values = values.into_iter().collect::<Vec<_>>();
    (!values.is_empty()).then(|| distribution(values))
}

fn percentile(values: &[f64], percentile: f64) -> f64 {
    let rank = (percentile * values.len() as f64).ceil() as usize;
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn read_cases(path: &Path) -> Vec<Case> {
    fs::read_to_string(path)
        .expect("read cases")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let columns = line.split('\t').collect::<Vec<_>>();
            assert_eq!(columns.len(), 4, "case must have four TSV columns");
            Case {
                id: columns[0].to_owned(),
                category: columns[1].to_owned(),
                query: columns[2].to_owned(),
                target: PathBuf::from(columns[3]),
            }
        })
        .collect()
}

fn read_recents(path: &Path) -> Vec<PathBuf> {
    let value: Value = serde_json::from_slice(&fs::read(path).expect("read Recents cache"))
        .expect("parse Recents cache");
    value["items"]
        .as_array()
        .expect("Recents items array")
        .iter()
        .filter_map(|item| item["path"].as_str())
        .map(PathBuf::from)
        .collect()
}

fn read_journal(path: &Path) -> Vec<PathBuf> {
    fs::read_to_string(path)
        .expect("read Visit Journal")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|record| record["path"].as_str().map(PathBuf::from))
        .collect()
}

fn read_pinned(path: &Path) -> Vec<PathBuf> {
    if !path.exists() {
        return Vec::new();
    }
    let value: Value = serde_json::from_slice(&fs::read(path).expect("read Pinned Tabs"))
        .expect("parse Pinned Tabs");
    value
        .as_array()
        .expect("Pinned Tabs array")
        .iter()
        .filter_map(|tab| tab["anchorPath"].as_str())
        .map(PathBuf::from)
        .collect()
}

fn parse_args() -> Config {
    let mut values = env::args().skip(1);
    let mut keyed = BTreeMap::<String, String>::new();
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .unwrap_or_else(|| panic!("missing value for {flag}"));
        keyed.insert(flag, value);
    }
    let required =
        |flag: &str| PathBuf::from(keyed.get(flag).unwrap_or_else(|| panic!("missing {flag}")));
    Config {
        index: required("--index"),
        root: required("--root"),
        current: required("--current"),
        recents: required("--recents"),
        journal: required("--journal"),
        pinned: keyed.get("--pinned").map(PathBuf::from),
        cases: required("--cases"),
        samples: keyed
            .get("--samples")
            .map_or(10, |value| value.parse().expect("--samples integer")),
        qgram_index: keyed.get("--qgram-index").map(PathBuf::from),
    }
}
