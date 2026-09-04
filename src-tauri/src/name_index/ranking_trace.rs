//! Local, replayable Ranking Traces (SPEC §6.5, §11).
//!
//! The search path only sends small owned messages. One event-driven worker owns the append
//! file and a single 300 ms detailed-trace debounce; there is no polling and no thread per
//! keystroke. Compact query records and action records append immediately. The newest final
//! result snapshot replaces an older pending detail record until the query stays idle.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::Path,
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{
    query::{ScoreContributions, ScoreEvidence, SearchHit, WeightedFeature},
    ranker_config::RankerConfig,
};

const SCHEMA_VERSION: u32 = 2;
const DETAIL_IDLE: Duration = Duration::from_millis(300);
const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const MAINTENANCE_INTERVAL_MS: i64 = 24 * 60 * 60 * 1_000;
const LOG_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
const COMPACT_TARGET_BYTES: usize = 192 * 1024 * 1024;

#[derive(Clone)]
pub struct RankingTraces {
    sender: Sender<Event>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Query {
        schema_version: u32,
        timestamp_ms: i64,
        query: String,
        config_fingerprint: String,
        significant_chars: usize,
        global_requested: bool,
    },
    Results {
        schema_version: u32,
        timestamp_ms: i64,
        query: String,
        config_fingerprint: String,
        stage: String,
        backend_duration_ms: u64,
        scanned: usize,
        results: Vec<RankedItem>,
    },
    Action {
        schema_version: u32,
        timestamp_ms: i64,
        query: Option<String>,
        path: String,
        signal: String,
        config_fingerprint: String,
    },
}

impl Record {
    fn timestamp_ms(&self) -> i64 {
        match self {
            Self::Query { timestamp_ms, .. }
            | Self::Results { timestamp_ms, .. }
            | Self::Action { timestamp_ms, .. } => *timestamp_ms,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RankedItem {
    rank: usize,
    path: String,
    name: String,
    score: i64,
    contributions: ScoreContributions,
    evidence: ScoreEvidence,
    features: Vec<WeightedFeature>,
    tier: String,
    is_directory: bool,
}

enum Event {
    Append(Record),
    ScheduleDetail(Record),
    SnapshotConfig {
        fingerprint: String,
        formatted: String,
    },
}

impl RankingTraces {
    pub fn init(app_data_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(app_data_dir)?;
        let trace_path = app_data_dir.join("ranking_traces.ndjson");
        let snapshots = app_data_dir.join("ranker_snapshots");
        fs::create_dir_all(&snapshots)?;
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("beeline-ranking-traces".to_owned())
            .spawn(move || {
                crate::qos::set_qos(0x11);
                if let Err(error) = run_writer(&trace_path, &snapshots, receiver) {
                    eprintln!("ranking trace writer failed: {error}");
                }
            })?;
        Ok(Self { sender })
    }

    pub fn snapshot_config(&self, config: &RankerConfig) {
        let _ = self.sender.send(Event::SnapshotConfig {
            fingerprint: config.fingerprint(),
            formatted: serde_json::to_string_pretty(config)
                .expect("Ranker Configuration serializes"),
        });
    }

    pub fn query_started(
        &self,
        timestamp_ms: i64,
        query: &str,
        config: &RankerConfig,
        significant_chars: usize,
        global_requested: bool,
    ) {
        let _ = self.sender.send(Event::Append(Record::Query {
            schema_version: SCHEMA_VERSION,
            timestamp_ms,
            query: query.to_owned(),
            config_fingerprint: config.fingerprint(),
            significant_chars,
            global_requested,
        }));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn results_complete(
        &self,
        timestamp_ms: i64,
        query: &str,
        config: &RankerConfig,
        stage: &str,
        backend_duration_ms: u64,
        scanned: usize,
        hits: &[SearchHit],
    ) {
        let results = hits
            .iter()
            .enumerate()
            .map(|(index, hit)| RankedItem {
                rank: index + 1,
                path: hit.path.clone(),
                name: hit.name.clone(),
                score: hit.score,
                contributions: hit.contributions.clone(),
                evidence: hit.evidence.clone(),
                features: hit.evidence.weighted_features(config),
                tier: hit.tier.to_owned(),
                is_directory: hit.is_directory,
            })
            .collect();
        let _ = self.sender.send(Event::ScheduleDetail(Record::Results {
            schema_version: SCHEMA_VERSION,
            timestamp_ms,
            query: query.to_owned(),
            config_fingerprint: config.fingerprint(),
            stage: stage.to_owned(),
            backend_duration_ms,
            scanned,
            results,
        }));
    }

    pub fn action(
        &self,
        timestamp_ms: i64,
        query: Option<&str>,
        path: &str,
        signal: &str,
        config: &RankerConfig,
    ) {
        let _ = self.sender.send(Event::Append(Record::Action {
            schema_version: SCHEMA_VERSION,
            timestamp_ms,
            query: query.map(str::to_owned),
            path: path.to_owned(),
            signal: signal.to_owned(),
            config_fingerprint: config.fingerprint(),
        }));
    }
}

fn run_writer(
    trace_path: &Path,
    snapshots: &Path,
    receiver: mpsc::Receiver<Event>,
) -> io::Result<()> {
    let mut last_maintenance_ms = wall_clock_ms();
    compact_if_needed(trace_path, last_maintenance_ms)?;
    prune_snapshots(trace_path, snapshots)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(trace_path)?;
    let mut pending: Option<(Instant, Record)> = None;
    loop {
        let event = match pending.as_ref() {
            Some((deadline, _)) => {
                let timeout = deadline.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(timeout) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => {
                        let (_, record) = pending.take().expect("pending detail exists");
                        append(&mut file, &record)?;
                        compact_after_write(
                            trace_path,
                            snapshots,
                            &mut file,
                            record.timestamp_ms(),
                            &mut last_maintenance_ms,
                        )?;
                        None
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match receiver.recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            },
        };
        let Some(event) = event else {
            continue;
        };
        match event {
            Event::Append(record) => {
                if matches!(record, Record::Query { .. }) {
                    pending = None;
                }
                append(&mut file, &record)?;
                compact_after_write(
                    trace_path,
                    snapshots,
                    &mut file,
                    record.timestamp_ms(),
                    &mut last_maintenance_ms,
                )?;
            }
            Event::ScheduleDetail(record) => {
                pending = Some((Instant::now() + DETAIL_IDLE, record));
            }
            Event::SnapshotConfig {
                fingerprint,
                formatted,
            } => snapshot_config(snapshots, &fingerprint, &formatted)?,
        }
    }
    if let Some((_, record)) = pending {
        append(&mut file, &record)?;
    }
    file.flush()
}

fn append(file: &mut File, record: &Record) -> io::Result<()> {
    serde_json::to_writer(&mut *file, record).map_err(io::Error::other)?;
    file.write_all(b"\n")
}

fn snapshot_config(directory: &Path, fingerprint: &str, formatted: &str) -> io::Result<()> {
    let target = directory.join(format!("{fingerprint}.json"));
    if target.exists() {
        return Ok(());
    }
    let temp = directory.join(format!("{fingerprint}.json.tmp"));
    let mut output = File::create(&temp)?;
    output.write_all(formatted.as_bytes())?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    fs::rename(temp, target)
}

fn compact_after_write(
    path: &Path,
    snapshots: &Path,
    file: &mut File,
    newest_ms: i64,
    last_maintenance_ms: &mut i64,
) -> io::Result<()> {
    if file.metadata()?.len() <= LOG_BUDGET_BYTES
        && newest_ms.saturating_sub(*last_maintenance_ms) < MAINTENANCE_INTERVAL_MS
    {
        return Ok(());
    }
    file.flush()?;
    compact_if_needed(path, newest_ms)?;
    prune_snapshots(path, snapshots)?;
    *last_maintenance_ms = newest_ms;
    *file = OpenOptions::new().append(true).open(path)?;
    Ok(())
}

fn prune_snapshots(trace_path: &Path, directory: &Path) -> io::Result<()> {
    let mut referenced = std::collections::HashSet::new();
    if let Ok(file) = File::open(trace_path) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(fingerprint) = value
                    .get("config_fingerprint")
                    .and_then(serde_json::Value::as_str)
                {
                    referenced.insert(fingerprint.to_owned());
                }
            }
        }
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if !referenced.contains(stem) {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn compact_if_needed(path: &Path, newest_ms: i64) -> io::Result<()> {
    match fs::metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let cutoff = newest_ms.saturating_sub(RETENTION_MS);
    let mut retained = Vec::new();
    let mut bytes = 0usize;
    for line in BufReader::new(File::open(path)?)
        .lines()
        .map_while(Result::ok)
    {
        let timestamp = serde_json::from_str::<serde_json::Value>(&line)
            .ok()
            .and_then(|value| {
                value
                    .get("timestamp_ms")
                    .and_then(serde_json::Value::as_i64)
            })
            .unwrap_or(i64::MIN);
        if timestamp >= cutoff {
            bytes = bytes.saturating_add(line.len() + 1);
            retained.push(line);
        }
    }
    let mut drop_count = 0usize;
    while bytes > COMPACT_TARGET_BYTES && drop_count < retained.len() {
        bytes = bytes.saturating_sub(retained[drop_count].len() + 1);
        drop_count += 1;
    }
    let temp = path.with_extension("ndjson.tmp");
    let mut output = File::create(&temp)?;
    for line in &retained[drop_count..] {
        output.write_all(line.as_bytes())?;
        output.write_all(b"\n")?;
    }
    output.sync_all()?;
    fs::rename(temp, path)
}

fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub fn replay(
    path: &Path,
    alternative: Option<&RankerConfig>,
) -> Result<serde_json::Value, String> {
    let file = File::open(path)
        .map_err(|error| format!("cannot open Ranking Traces {}: {error}", path.display()))?;
    let mut queries = 0usize;
    let mut result_snapshots = 0usize;
    let mut actions = 0usize;
    let mut ranked_items = 0usize;
    let mut configs = HashMap::<String, RankerConfig>::new();
    let mut missing_snapshots = HashSet::<String>::new();
    let mut changed_snapshots = 0usize;
    let mut moved_items = 0usize;
    let mut rank_changes = Vec::new();
    for (line_number, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|error| {
            format!(
                "cannot read Ranking Traces {} line {}: {error}",
                path.display(),
                line_number + 1
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<Record>(&line).map_err(|error| {
            format!(
                "invalid Ranking Trace {} line {}: {error}",
                path.display(),
                line_number + 1
            )
        })?;
        let schema_version = match &record {
            Record::Query { schema_version, .. }
            | Record::Results { schema_version, .. }
            | Record::Action { schema_version, .. } => *schema_version,
        };
        if schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported Ranking Trace schemaVersion {schema_version} on line {}; expected {SCHEMA_VERSION}",
                line_number + 1
            ));
        }
        match record {
            Record::Query { .. } => queries += 1,
            Record::Action { .. } => actions += 1,
            Record::Results {
                query,
                config_fingerprint,
                results,
                ..
            } => {
                result_snapshots += 1;
                ranked_items += results.len();
                if !configs.contains_key(&config_fingerprint) {
                    let snapshot_path = path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join("ranker_snapshots")
                        .join(format!("{config_fingerprint}.json"));
                    match fs::read_to_string(&snapshot_path)
                        .map_err(|error| error.to_string())
                        .and_then(|text| RankerConfig::parse(&text))
                    {
                        Ok(config) => {
                            configs.insert(config_fingerprint.clone(), config);
                        }
                        Err(_) => {
                            missing_snapshots.insert(config_fingerprint.clone());
                        }
                    }
                }
                let original_config = configs.get(&config_fingerprint);
                let mut previous_score = i64::MAX;
                for (index, result) in results.iter().enumerate() {
                    if result.rank != index + 1 {
                        return Err(format!(
                            "Ranking Trace line {} has non-contiguous rank {}",
                            line_number + 1,
                            result.rank
                        ));
                    }
                    if result.score != result.contributions.total() {
                        return Err(format!(
                            "Ranking Trace line {} rank {} score {} does not equal contribution total {}",
                            line_number + 1,
                            result.rank,
                            result.score,
                            result.contributions.total()
                        ));
                    }
                    let feature_total = result.features.iter().fold(0i64, |total, feature| {
                        total.saturating_add(feature.contribution)
                    });
                    if feature_total != result.score {
                        return Err(format!(
                            "Ranking Trace line {} rank {} weighted feature total {} does not equal score {}",
                            line_number + 1,
                            result.rank,
                            feature_total,
                            result.score
                        ));
                    }
                    if result.score > previous_score {
                        return Err(format!(
                            "Ranking Trace line {} is not ordered at rank {}",
                            line_number + 1,
                            result.rank
                        ));
                    }
                    if let Some(config) = original_config {
                        let reconstructed = result.evidence.contributions(config);
                        if reconstructed != result.contributions {
                            return Err(format!(
                                "Ranking Trace line {} rank {} evidence does not reconstruct recorded contributions",
                                line_number + 1,
                                result.rank
                            ));
                        }
                    }
                    previous_score = result.score;
                }
                if let Some(alternative) = alternative {
                    let Some(_original) = original_config else {
                        continue;
                    };
                    let mut reranked = results
                        .iter()
                        .enumerate()
                        .map(|(old_index, item)| {
                            (
                                old_index,
                                item.evidence.contributions(alternative).total(),
                                item.path.len(),
                                item.path.as_str(),
                            )
                        })
                        .collect::<Vec<_>>();
                    reranked.sort_by(|left, right| {
                        right
                            .1
                            .cmp(&left.1)
                            .then_with(|| left.2.cmp(&right.2))
                            .then_with(|| left.3.cmp(right.3))
                    });
                    let moved = reranked
                        .iter()
                        .enumerate()
                        .filter(|(new_index, item)| *new_index != item.0)
                        .count();
                    if moved > 0 {
                        changed_snapshots += 1;
                        moved_items += moved;
                        for (new_index, (old_index, new_score, _, path)) in
                            reranked.iter().enumerate()
                        {
                            if new_index == *old_index || rank_changes.len() >= 1_000 {
                                continue;
                            }
                            rank_changes.push(serde_json::json!({
                                "query": query,
                                "path": path,
                                "oldRank": old_index + 1,
                                "newRank": new_index + 1,
                                "oldScore": results[*old_index].score,
                                "newScore": new_score,
                                "scoreDelta": new_score.saturating_sub(results[*old_index].score),
                            }));
                        }
                    }
                }
            }
        }
    }
    let mut summary = serde_json::json!({
        "valid": true,
        "queries": queries,
        "resultSnapshots": result_snapshots,
        "actions": actions,
        "rankedItems": ranked_items,
        "missingConfigSnapshots": missing_snapshots,
    });
    if let Some(alternative) = alternative {
        summary["alternativeFingerprint"] = alternative.fingerprint().into();
        summary["changedResultSnapshots"] = changed_snapshots.into();
        summary["movedItems"] = moved_items.into();
        summary["rankChanges"] = rank_changes.into();
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "beeline_ranking_trace_{}_{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn config_snapshot_is_content_addressed() {
        let dir = temp_dir();
        let config = RankerConfig::default();
        snapshot_config(&dir, &config.fingerprint(), "first").expect("first");
        snapshot_config(&dir, &config.fingerprint(), "second").expect("second");
        let text = fs::read_to_string(dir.join(format!("{}.json", config.fingerprint())))
            .expect("snapshot");
        assert_eq!(text, "first\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn append_writes_replayable_tagged_records() {
        let dir = temp_dir();
        let path = dir.join("trace.ndjson");
        let mut file = File::create(&path).expect("create");
        append(
            &mut file,
            &Record::Query {
                schema_version: SCHEMA_VERSION,
                timestamp_ms: 10,
                query: "methodology".to_owned(),
                config_fingerprint: "abc".to_owned(),
                significant_chars: 11,
                global_requested: true,
            },
        )
        .expect("append");
        drop(file);
        let value: serde_json::Value =
            serde_json::from_str(fs::read_to_string(path).expect("read").trim()).expect("json");
        assert_eq!(value["kind"], "query");
        assert_eq!(value["query"], "methodology");
        let summary = replay(&dir.join("trace.ndjson"), None).expect("replay");
        assert_eq!(summary["queries"], 1);
        assert_eq!(summary["resultSnapshots"], 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn replay_rejects_a_score_that_disagrees_with_contributions() {
        let dir = temp_dir();
        let path = dir.join("trace.ndjson");
        let mut file = File::create(&path).expect("create");
        append(
            &mut file,
            &Record::Results {
                schema_version: SCHEMA_VERSION,
                timestamp_ms: 10,
                query: "report".to_owned(),
                config_fingerprint: "abc".to_owned(),
                stage: "fuzzy".to_owned(),
                backend_duration_ms: 5,
                scanned: 1,
                results: vec![RankedItem {
                    rank: 1,
                    path: "/tmp/report".to_owned(),
                    name: "report".to_owned(),
                    score: 7,
                    contributions: ScoreContributions {
                        text_match: 8,
                        ..ScoreContributions::default()
                    },
                    evidence: ScoreEvidence {
                        text_match: crate::name_index::query::TextMatchEvidence {
                            feature: crate::name_index::query::TextMatchFeature::PrefixName,
                            ..crate::name_index::query::TextMatchEvidence::default()
                        },
                        tier: "normal".to_owned(),
                        ..ScoreEvidence::default()
                    },
                    features: Vec::new(),
                    tier: "normal".to_owned(),
                    is_directory: false,
                }],
            },
        )
        .expect("append");
        drop(file);
        assert!(replay(&path, None).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn alternative_replay_reweights_decomposed_evidence() {
        let dir = temp_dir();
        let snapshots = dir.join("ranker_snapshots");
        fs::create_dir(&snapshots).expect("snapshots");
        let original = RankerConfig::default();
        original.validate().expect("original config");
        let fingerprint = original.fingerprint();
        snapshot_config(
            &snapshots,
            &fingerprint,
            &serde_json::to_string_pretty(&original).expect("serialize"),
        )
        .expect("snapshot");

        let first_evidence = ScoreEvidence {
            text_match: crate::name_index::query::TextMatchEvidence {
                feature: crate::name_index::query::TextMatchFeature::ExactName,
                ..crate::name_index::query::TextMatchEvidence::default()
            },
            tier: "normal".to_owned(),
            ..ScoreEvidence::default()
        };
        let second_evidence = ScoreEvidence {
            text_match: crate::name_index::query::TextMatchEvidence {
                feature: crate::name_index::query::TextMatchFeature::PrefixName,
                path_scope: true,
                ..crate::name_index::query::TextMatchEvidence::default()
            },
            tier: "normal".to_owned(),
            ..ScoreEvidence::default()
        };
        let first_contributions = first_evidence.contributions(&original);
        let second_contributions = second_evidence.contributions(&original);
        let first_features = first_evidence.weighted_features(&original);
        let second_features = second_evidence.weighted_features(&original);
        let path = dir.join("ranking_traces.ndjson");
        let mut file = File::create(&path).expect("create");
        append(
            &mut file,
            &Record::Results {
                schema_version: SCHEMA_VERSION,
                timestamp_ms: 10,
                query: "report".to_owned(),
                config_fingerprint: fingerprint,
                stage: "fuzzy".to_owned(),
                backend_duration_ms: 5,
                scanned: 2,
                results: vec![
                    RankedItem {
                        rank: 1,
                        path: "/tmp/a".to_owned(),
                        name: "a".to_owned(),
                        score: first_contributions.total(),
                        contributions: first_contributions,
                        evidence: first_evidence,
                        features: first_features,
                        tier: "normal".to_owned(),
                        is_directory: false,
                    },
                    RankedItem {
                        rank: 2,
                        path: "/tmp/b".to_owned(),
                        name: "b".to_owned(),
                        score: second_contributions.total(),
                        contributions: second_contributions,
                        evidence: second_evidence,
                        features: second_features,
                        tier: "normal".to_owned(),
                        is_directory: false,
                    },
                ],
            },
        )
        .expect("append");
        drop(file);

        let mut alternative = original.clone();
        alternative.text_match.path_scope = 2_000_000;
        let summary = replay(&path, Some(&alternative)).expect("alternative replay");
        assert_eq!(summary["changedResultSnapshots"], 1);
        assert_eq!(summary["movedItems"], 2);
        assert_eq!(summary["rankChanges"].as_array().unwrap().len(), 2);
        assert_eq!(summary["missingConfigSnapshots"], serde_json::json!([]));
        let _ = fs::remove_dir_all(dir);
    }
}
