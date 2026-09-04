//! Headless, randomized benchmark for the production Search v2 core.
//!
//! The benchmark is deliberately reached from `main` before Tauri is initialized, so running it
//! never creates a window or activates the app. It loads the same Name Index, q-gram sidecar,
//! Visit Journal, aliases, Working Set builder, matcher, and ranker as the desktop command.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{atomic::AtomicU64, Arc, RwLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::Value;

use super::{
    alias::AliasDictionary,
    crawl,
    junk::JunkPatterns,
    model::IndexData,
    persist,
    qgram::QGramIndex,
    query::{self, RankContext, SearchHit},
    visit_journal::VisitJournal,
    working_set, WorkingSet,
};

const RESULT_LIMIT: usize = 50;
const REPORT_SCHEMA: u32 = 2;

#[derive(Clone)]
struct Case {
    id: String,
    category: String,
    query: String,
    target: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexState {
    Base,
    Reconciled,
}

impl IndexState {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "base" => Ok(Self::Base),
            "reconciled" => Ok(Self::Reconciled),
            _ => Err(format!(
                "invalid --state {value:?}; expected base or reconciled"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Reconciled => "reconciled",
        }
    }
}

struct Config {
    index: PathBuf,
    qgram: PathBuf,
    root: PathBuf,
    app_data: PathBuf,
    current: Option<PathBuf>,
    cases: PathBuf,
    samples: usize,
    warmups: usize,
    seed: u64,
    session: String,
    state: IndexState,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    order: usize,
    repetition: usize,
    working_set_ms: f64,
    priority_retrieval_ms: Option<f64>,
    priority_verify_ms: Option<f64>,
    priority_candidates: usize,
    priority_results: usize,
    first_useful_ms: Option<f64>,
    target_first_ms: Option<f64>,
    candidate_retrieval_ms: f64,
    candidate_prepare_ms: f64,
    first_global_wave_ms: Option<f64>,
    verify_rank_ms: f64,
    total_ms: f64,
    candidate_slots: usize,
    posting_visits: usize,
    verified_items: usize,
    result_count: usize,
    working_result_count: usize,
    working_target_rank: Option<usize>,
    target_rank: Option<usize>,
    top_ten_fingerprint: u64,
    partial_waves: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Distribution {
    p50: f64,
    p90: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseReport {
    id: String,
    category: String,
    query: String,
    target: String,
    target_misses: usize,
    working_target_hits: usize,
    target_rank_min: Option<usize>,
    target_rank_max: Option<usize>,
    top_ten_variants: usize,
    working_set: Distribution,
    priority_retrieval: Option<Distribution>,
    priority_verify: Option<Distribution>,
    priority_candidates: Distribution,
    priority_results: Distribution,
    first_useful: Option<Distribution>,
    target_first: Option<Distribution>,
    candidate_retrieval: Distribution,
    candidate_prepare: Distribution,
    first_global_wave: Option<Distribution>,
    verify_rank: Distribution,
    total: Distribution,
    candidate_slots: Distribution,
    posting_visits: Distribution,
    verified_items: Distribution,
    observations: Vec<Observation>,
    final_top_hits: Vec<SearchHit>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    schema_version: u32,
    generated_at_epoch_ms: u128,
    session: String,
    seed: u64,
    state: &'static str,
    samples_per_case: usize,
    warmups_per_case: usize,
    randomized: bool,
    available_parallelism: usize,
    index_path: String,
    index_bytes: u64,
    qgram_path: String,
    qgram_bytes: u64,
    base_items: usize,
    live_items: usize,
    base_slots: usize,
    live_slots: usize,
    overlay_slots: usize,
    current_location: Option<String>,
    working_set_items: usize,
    index_load_ms: f64,
    reconcile_ms: Option<f64>,
    benchmark_ms: f64,
    cases: Vec<CaseReport>,
    limitations: Vec<&'static str>,
}

struct SampleResult {
    observation: Observation,
    hits: Vec<SearchHit>,
}

pub fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let config = parse_args(arguments)?;
    let total_started = Instant::now();
    let index_load_started = Instant::now();
    let data = Arc::new(RwLock::new(
        persist::load(&config.index, &config.root)
            .ok_or_else(|| "failed to load or validate Name Index v4".to_owned())?,
    ));
    let index_load_ms = elapsed_ms(index_load_started);

    let (settings, _) = crate::settings::read(&config.app_data);
    let junk = JunkPatterns::from_names(settings.junk_patterns);
    let aliases = AliasDictionary::from_pairs(
        settings
            .aliases
            .into_iter()
            .map(|entry| (entry.word, entry.path)),
        &config.root,
    );
    let reconcile_ms = if config.state == IndexState::Reconciled {
        let started = Instant::now();
        crawl::diff_rescan(&data, config.root.clone(), &junk, None);
        Some(elapsed_ms(started))
    } else {
        None
    };

    let journal = VisitJournal::load(&config.app_data)
        .map_err(|error| format!("failed to load Visit Journal: {error}"))?;
    let recent_paths = read_recents(&config.app_data.join("recents_cache.json"));
    let pinned_paths = read_pinned(&config.app_data.join("pinned_tabs.json"));
    let aggregate = journal.aggregate();
    let memory = super::search_memory::MemoryEvidence::default();

    let (source_hash, base_items, live_items, base_slots, live_slots, working) = {
        let index = data.read().expect("name index lock poisoned");
        let base_slots = index.base_entry_count();
        (
            index
                .base_content_hash()
                .ok_or_else(|| "Name Index has no immutable base".to_owned())?,
            base_slots,
            index.len(),
            base_slots,
            index.slot_len(),
            working_set(
                &index,
                config.current.as_deref(),
                &pinned_paths,
                &recent_paths,
                &aggregate,
                &memory,
            ),
        )
    };
    let WorkingSet {
        slots: working_slots,
        retrieval,
    } = working;
    let qgram = QGramIndex::open(&config.qgram, source_hash, base_slots).ok_or_else(|| {
        "q-gram sidecar is missing, invalid, or belongs to another Name Index generation".to_owned()
    })?;
    let cases = read_cases(&config.cases)?;
    if cases.is_empty() {
        return Err("case file contains no benchmark cases".to_owned());
    }
    if let Some(case) = cases.iter().find(|case| {
        super::qgram::significant_len(&case.query) < 5 || !super::qgram::supports_query(&case.query)
    }) {
        return Err(format!(
            "case {} does not use production q-gram retrieval; benchmark it in a separately labelled exact-fallback suite",
            case.id
        ));
    }

    eprintln!(
        "Search v2 benchmark: {} cases × {} samples, session {}, state {}",
        cases.len(),
        config.samples,
        config.session,
        config.state.as_str()
    );

    let index = data.read().expect("name index lock poisoned");
    let context = RankContext {
        journal: &aggregate,
        aliases: &aliases,
        retrieval,
        memory: super::search_memory::MemoryEvidence::empty(),
        config: RankContext::empty().config,
    };
    for case in &cases {
        for _ in 0..config.warmups {
            let _ = measure(&index, &qgram, &context, &working_slots, case, 0, 0)?;
        }
    }

    let mut schedule = Vec::with_capacity(cases.len() * config.samples);
    for case_index in 0..cases.len() {
        for repetition in 0..config.samples {
            schedule.push((case_index, repetition));
        }
    }
    SplitMix64::new(config.seed).shuffle(&mut schedule);

    let benchmark_started = Instant::now();
    let mut samples: Vec<Vec<SampleResult>> = (0..cases.len()).map(|_| Vec::new()).collect();
    for (order, (case_index, repetition)) in schedule.into_iter().enumerate() {
        let measured = measure(
            &index,
            &qgram,
            &context,
            &working_slots,
            &cases[case_index],
            order,
            repetition,
        )?;
        samples[case_index].push(measured);
        if (order + 1).is_multiple_of(100) || order + 1 == cases.len() * config.samples {
            eprintln!(
                "  {}/{} observations ({:.1} s)",
                order + 1,
                cases.len() * config.samples,
                benchmark_started.elapsed().as_secs_f64()
            );
        }
    }
    let benchmark_ms = elapsed_ms(benchmark_started);

    let reports = cases
        .into_iter()
        .zip(samples)
        .map(|(case, samples)| case_report(case, samples))
        .collect();
    let report = Report {
        schema_version: REPORT_SCHEMA,
        generated_at_epoch_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis()),
        session: config.session,
        seed: config.seed,
        state: config.state.as_str(),
        samples_per_case: config.samples,
        warmups_per_case: config.warmups,
        randomized: true,
        available_parallelism: std::thread::available_parallelism().map_or(1, usize::from),
        index_path: display(&config.index),
        index_bytes: fs::metadata(&config.index)
            .map_err(|error| format!("failed to stat Name Index: {error}"))?
            .len(),
        qgram_path: display(&config.qgram),
        qgram_bytes: fs::metadata(&config.qgram)
            .map_err(|error| format!("failed to stat q-gram sidecar: {error}"))?
            .len(),
        base_items,
        live_items,
        base_slots,
        live_slots,
        overlay_slots: live_slots.saturating_sub(base_slots),
        current_location: config.current.as_ref().map(|path| display(path)),
        working_set_items: working_slots.len(),
        index_load_ms,
        reconcile_ms,
        benchmark_ms,
        cases: reports,
        limitations: vec![
            "Headless production core only: Tauri IPC, React commit, and paint are not measured",
            "Post-reboot cold-page behavior requires a separately labelled reboot session",
            "Process memory and page-fault counters are captured by the outer /usr/bin/time invocation",
        ],
    };
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer_pretty(&mut output, &report)
        .map_err(|error| format!("failed to encode benchmark report: {error}"))?;
    writeln!(output).map_err(|error| format!("failed to finish benchmark report: {error}"))?;
    eprintln!(
        "Search v2 benchmark complete in {:.1} s (total {:.1} s)",
        benchmark_ms / 1_000.0,
        total_started.elapsed().as_secs_f64()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn measure(
    index: &IndexData,
    qgram: &QGramIndex,
    context: &RankContext<'_>,
    working_slots: &[u32],
    case: &Case,
    order: usize,
    repetition: usize,
) -> Result<SampleResult, String> {
    let generation = AtomicU64::new(1);
    let cancel = query::Cancel::new(&generation, 1);
    let started = Instant::now();
    let working = query::run_fuzzy(
        index,
        context,
        &case.query,
        RESULT_LIMIT,
        working_slots,
        Some(&cancel),
        true,
    );
    if working.aborted {
        return Err(format!("working-set search aborted for {}", case.id));
    }
    let working_set_ms = elapsed_ms(started);
    let target = case.target.to_string_lossy();
    let working_target_rank = working
        .hits
        .iter()
        .position(|hit| hit.path == target)
        .map(|position| position + 1);
    let mut first_useful_ms = (!working.hits.is_empty()).then_some(working_set_ms);
    let mut target_first_ms = working_target_rank.map(|_| working_set_ms);

    let retrieval_started = Instant::now();
    let mut priority_retrieval_ms = None;
    let mut priority_verify_ms = None;
    let mut priority_candidates = 0usize;
    let mut priority_results = 0usize;
    let mut first_global_wave_ms = None;
    let mut partial_waves = 0usize;
    if working.hits.is_empty() {
        let priority_started = Instant::now();
        if let Some(seed) = qgram.ordered_tail_literal_candidates(&case.query, &cancel) {
            if seed.aborted {
                return Err(format!("priority retrieval aborted for {}", case.id));
            }
            let mut priority_slots = seed.slots;
            let overlay = query::ordered_tail_literal_slots(index, &case.query, Some(&cancel));
            if overlay.aborted {
                return Err(format!("priority overlay scan aborted for {}", case.id));
            }
            priority_slots.extend(overlay.slots);
            priority_slots.sort_unstable();
            priority_slots.dedup();
            priority_candidates = priority_slots.len();
            priority_retrieval_ms = Some(elapsed_ms(priority_started));
            let verify_started = Instant::now();
            let priority = query::run_fuzzy(
                index,
                context,
                &case.query,
                RESULT_LIMIT,
                &priority_slots,
                Some(&cancel),
                false,
            );
            priority_verify_ms = Some(elapsed_ms(verify_started));
            if priority.aborted {
                return Err(format!("priority verification aborted for {}", case.id));
            }
            priority_results = priority.hits.len();
            if !priority.hits.is_empty() {
                partial_waves += 1;
                let elapsed = elapsed_ms(started);
                first_global_wave_ms = Some(elapsed);
                first_useful_ms = Some(elapsed);
                if target_first_ms.is_none() && priority.hits.iter().any(|hit| hit.path == target) {
                    target_first_ms = Some(elapsed);
                }
            }
        }
    }
    let candidate_set = qgram.candidates(&case.query, &cancel);
    let candidate_retrieval_ms = elapsed_ms(retrieval_started);
    if candidate_set.aborted {
        return Err(format!("q-gram retrieval aborted for {}", case.id));
    }

    let prepare_started = Instant::now();
    let mut candidate_slots = candidate_set.slots;
    candidate_slots.extend(index.overlay_entry_slots());
    candidate_slots.extend(working_slots.iter().copied());
    candidate_slots.sort_unstable();
    candidate_slots.dedup();
    let candidate_prepare_ms = elapsed_ms(prepare_started);

    let verify_started = Instant::now();
    let mut on_partial = |hits: Vec<SearchHit>, _scanned: usize| {
        if priority_results > 0 || hits.is_empty() {
            return;
        }
        partial_waves += 1;
        let elapsed = elapsed_ms(started);
        first_global_wave_ms.get_or_insert(elapsed);
        if target_first_ms.is_none() && hits.iter().any(|hit| hit.path == target) {
            target_first_ms = Some(elapsed);
        }
    };
    let global = query::run_fuzzy_streaming(
        index,
        context,
        &case.query,
        RESULT_LIMIT,
        &candidate_slots,
        Some(&cancel),
        false,
        &mut on_partial,
    );
    let verify_rank_ms = elapsed_ms(verify_started);
    if global.aborted {
        return Err(format!("global verification aborted for {}", case.id));
    }
    if first_global_wave_ms.is_none() && !global.hits.is_empty() {
        first_global_wave_ms = Some(elapsed_ms(started));
    }
    first_useful_ms = first_useful_ms.or(first_global_wave_ms);
    let total_ms = elapsed_ms(started);
    let target_rank = global
        .hits
        .iter()
        .position(|hit| hit.path == target)
        .map(|position| position + 1);
    if target_first_ms.is_none() && target_rank.is_some() {
        target_first_ms = Some(total_ms);
    }

    Ok(SampleResult {
        observation: Observation {
            order,
            repetition,
            working_set_ms,
            priority_retrieval_ms,
            priority_verify_ms,
            priority_candidates,
            priority_results,
            first_useful_ms,
            target_first_ms,
            candidate_retrieval_ms,
            candidate_prepare_ms,
            first_global_wave_ms,
            verify_rank_ms,
            total_ms,
            candidate_slots: candidate_slots.len(),
            posting_visits: candidate_set.posting_visits,
            verified_items: global.scanned,
            result_count: global.hits.len(),
            working_result_count: working.hits.len(),
            working_target_rank,
            target_rank,
            top_ten_fingerprint: fingerprint(&global.hits, 10),
            partial_waves,
        },
        hits: global.hits,
    })
}

fn case_report(case: Case, samples: Vec<SampleResult>) -> CaseReport {
    let final_top_hits = samples
        .last()
        .map(|sample| sample.hits.iter().take(5).cloned().collect())
        .unwrap_or_default();
    let target_ranks = samples
        .iter()
        .filter_map(|sample| sample.observation.target_rank)
        .collect::<Vec<_>>();
    let top_ten_variants = samples
        .iter()
        .map(|sample| sample.observation.top_ten_fingerprint)
        .collect::<BTreeSet<_>>()
        .len();
    let observations = samples
        .into_iter()
        .map(|sample| sample.observation)
        .collect::<Vec<_>>();
    CaseReport {
        id: case.id,
        category: case.category,
        query: case.query,
        target: display(&case.target),
        target_misses: observations.len().saturating_sub(target_ranks.len()),
        working_target_hits: observations
            .iter()
            .filter(|sample| sample.working_target_rank.is_some())
            .count(),
        target_rank_min: target_ranks.iter().copied().min(),
        target_rank_max: target_ranks.iter().copied().max(),
        top_ten_variants,
        working_set: distribution(observations.iter().map(|sample| sample.working_set_ms)),
        priority_retrieval: optional_distribution(
            observations
                .iter()
                .filter_map(|sample| sample.priority_retrieval_ms),
        ),
        priority_verify: optional_distribution(
            observations
                .iter()
                .filter_map(|sample| sample.priority_verify_ms),
        ),
        priority_candidates: distribution(
            observations
                .iter()
                .map(|sample| sample.priority_candidates as f64),
        ),
        priority_results: distribution(
            observations
                .iter()
                .map(|sample| sample.priority_results as f64),
        ),
        first_useful: optional_distribution(
            observations
                .iter()
                .filter_map(|sample| sample.first_useful_ms),
        ),
        target_first: optional_distribution(
            observations
                .iter()
                .filter_map(|sample| sample.target_first_ms),
        ),
        candidate_retrieval: distribution(
            observations
                .iter()
                .map(|sample| sample.candidate_retrieval_ms),
        ),
        candidate_prepare: distribution(
            observations
                .iter()
                .map(|sample| sample.candidate_prepare_ms),
        ),
        first_global_wave: optional_distribution(
            observations
                .iter()
                .filter_map(|sample| sample.first_global_wave_ms),
        ),
        verify_rank: distribution(observations.iter().map(|sample| sample.verify_rank_ms)),
        total: distribution(observations.iter().map(|sample| sample.total_ms)),
        candidate_slots: distribution(
            observations
                .iter()
                .map(|sample| sample.candidate_slots as f64),
        ),
        posting_visits: distribution(
            observations
                .iter()
                .map(|sample| sample.posting_visits as f64),
        ),
        verified_items: distribution(
            observations
                .iter()
                .map(|sample| sample.verified_items as f64),
        ),
        observations,
        final_top_hits,
    }
}

fn distribution(values: impl IntoIterator<Item = f64>) -> Distribution {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    assert!(!values.is_empty(), "distribution requires samples");
    Distribution {
        p50: percentile(&values, 0.50),
        p90: percentile(&values, 0.90),
        p95: percentile(&values, 0.95),
        p99: percentile(&values, 0.99),
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

fn fingerprint(hits: &[SearchHit], count: usize) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for hit in hits.iter().take(count) {
        for byte in hit.path.as_bytes().iter().chain(std::iter::once(&0xff)) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_cases(path: &Path) -> Result<Vec<Case>, String> {
    fs::read_to_string(path)
        .map_err(|error| format!("failed to read cases: {error}"))?
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let columns = line.split('\t').collect::<Vec<_>>();
            if columns.len() != 4 {
                return Err(format!("case must have four TSV columns: {line}"));
            }
            Ok(Case {
                id: columns[0].to_owned(),
                category: columns[1].to_owned(),
                query: columns[2].to_owned(),
                target: PathBuf::from(columns[3]),
            })
        })
        .collect()
}

fn read_recents(path: &Path) -> Vec<PathBuf> {
    read_json(path)
        .and_then(|value| value.get("items").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| item.get("path").and_then(Value::as_str).map(PathBuf::from))
        .collect()
}

fn read_pinned(path: &Path) -> Vec<PathBuf> {
    read_json(path)
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| {
            item.get("anchorPath")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .collect()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Config, String> {
    let mut arguments = arguments.into_iter();
    let mut keyed = BTreeMap::<String, OsString>::new();
    while let Some(flag) = arguments.next() {
        let flag = flag
            .into_string()
            .map_err(|_| "benchmark flag is not valid UTF-8".to_owned())?;
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        if keyed.insert(flag.clone(), value).is_some() {
            return Err(format!("duplicate benchmark flag {flag}"));
        }
    }
    let required_path = |flag: &str| {
        keyed
            .get(flag)
            .map(PathBuf::from)
            .ok_or_else(|| format!("missing {flag}"))
    };
    let required_string = |flag: &str| {
        keyed
            .get(flag)
            .map(|value| value.to_string_lossy().into_owned())
            .ok_or_else(|| format!("missing {flag}"))
    };
    let parse_usize = |flag: &str, default: usize| -> Result<usize, String> {
        keyed.get(flag).map_or(Ok(default), |value| {
            value
                .to_string_lossy()
                .parse()
                .map_err(|_| format!("{flag} must be a positive integer"))
        })
    };
    let parse_u64 = |flag: &str, default: u64| -> Result<u64, String> {
        keyed.get(flag).map_or(Ok(default), |value| {
            value
                .to_string_lossy()
                .parse()
                .map_err(|_| format!("{flag} must be an unsigned integer"))
        })
    };
    let samples = parse_usize("--samples", 200)?;
    if samples == 0 {
        return Err("--samples must be greater than zero".to_owned());
    }
    let warmups = parse_usize("--warmups", 1)?;
    let state = IndexState::parse(&required_string("--state")?)?;
    Ok(Config {
        index: required_path("--index")?,
        qgram: required_path("--qgram")?,
        root: required_path("--root")?,
        app_data: required_path("--app-data")?,
        current: keyed.get("--current").map(PathBuf::from),
        cases: required_path("--cases")?,
        samples,
        warmups,
        seed: parse_u64("--seed", 1)?,
        session: required_string("--session")?,
        state,
    })
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }

    fn shuffle<T>(&mut self, values: &mut [T]) {
        for end in (1..values.len()).rev() {
            let index = (self.next() % (end as u64 + 1)) as usize;
            values.swap(end, index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        let values = (1..=200).map(f64::from).collect::<Vec<_>>();
        assert_eq!(percentile(&values, 0.95), 190.0);
        assert_eq!(percentile(&values, 0.99), 198.0);
    }

    #[test]
    fn seeded_shuffle_is_repeatable_and_not_identity() {
        let mut first = (0..32).collect::<Vec<_>>();
        let mut second = first.clone();
        SplitMix64::new(42).shuffle(&mut first);
        SplitMix64::new(42).shuffle(&mut second);
        assert_eq!(first, second);
        assert_ne!(first, (0..32).collect::<Vec<_>>());
    }
}
