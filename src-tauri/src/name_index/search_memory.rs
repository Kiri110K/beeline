//! Persistent personal ranking evidence for Search v2 (SPEC §6.3, §11).
//!
//! The on-disk form is an append-only NDJSON event log. Recording happens through a
//! blocking Tauri task, never on the UI or search path. Loading folds valid lines into a
//! bounded in-memory aggregate; malformed or obsolete alpha records are ignored. When the
//! log reaches its byte budget it is compacted atomically to one record per aggregate.

use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Mutex, RwLock},
};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use super::ranker_config::RankerConfig;

const SCHEMA_VERSION: u32 = 1;
const LOG_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LEARNED_RECORDS: usize = 200_000;
const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalKind {
    ActionMenu,
    QuickLook,
    CompletedAction,
}

impl SignalKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "action_menu" => Some(Self::ActionMenu),
            "quick_look" => Some(Self::QuickLook),
            "completed_action" => Some(Self::CompletedAction),
            _ => None,
        }
    }

    fn points(self, config: &RankerConfig) -> u32 {
        match self {
            Self::ActionMenu => config.search_memory.action_menu_points,
            Self::QuickLook => config.search_memory.quick_look_points,
            Self::CompletedAction => config.search_memory.completed_action_points,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
enum Interpretation {
    Ordinary,
    Path,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LearnedRecord {
    schema_version: u32,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_path: Option<String>,
    query: Option<String>,
    interpretation: Option<Interpretation>,
    points: u32,
    timestamp_ms: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    points: u64,
    last_ms: i64,
}

impl Stats {
    fn apply(&mut self, points: u32, timestamp_ms: i64) {
        self.points = self.points.saturating_add(u64::from(points));
        self.last_ms = self.last_ms.max(timestamp_ms);
    }

    fn strength_milli(self, now_ms: i64, config: &RankerConfig) -> i64 {
        let saturated = self
            .points
            .saturating_mul(1_000)
            .checked_div(
                self.points
                    .saturating_add(config.search_memory.saturation_points),
            )
            .unwrap_or(0);
        let age_days = now_ms.saturating_sub(self.last_ms).max(0) / DAY_MS;
        let halvings = (age_days / config.search_memory.half_life_days).min(20) as u32;
        i64::try_from(saturated >> halvings).unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct AssociationKey {
    query: String,
    interpretation: Interpretation,
    path: String,
}

#[derive(Clone)]
enum LearnedKey {
    Usage(String),
    Association(AssociationKey),
}

#[derive(Default)]
struct Aggregate {
    usage: HashMap<String, Stats>,
    associations: HashMap<AssociationKey, Stats>,
}

impl Aggregate {
    fn apply(&mut self, record: &LearnedRecord) {
        if let Some(previous) = &record.previous_path {
            self.rebind(previous, &record.path);
            return;
        }
        match (&record.query, record.interpretation) {
            (Some(query), Some(interpretation)) if !query.is_empty() => {
                self.associations
                    .entry(AssociationKey {
                        query: query.clone(),
                        interpretation,
                        path: record.path.clone(),
                    })
                    .or_default()
                    .apply(record.points, record.timestamp_ms);
            }
            (None, None) => self
                .usage
                .entry(record.path.clone())
                .or_default()
                .apply(record.points, record.timestamp_ms),
            _ => {}
        }
    }

    fn rebind(&mut self, previous: &str, next: &str) {
        let usage = self
            .usage
            .keys()
            .filter_map(|path| rebind_path(path, previous, next).map(|next| (path.clone(), next)))
            .collect::<Vec<_>>();
        for (old_path, new_path) in usage {
            if let Some(stats) = self.usage.remove(&old_path) {
                merge_stats(self.usage.entry(new_path).or_default(), stats);
            }
        }
        let associations = self
            .associations
            .keys()
            .filter_map(|key| {
                rebind_path(&key.path, previous, next).map(|path| {
                    (
                        key.clone(),
                        AssociationKey {
                            query: key.query.clone(),
                            interpretation: key.interpretation,
                            path,
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        for (old_key, new_key) in associations {
            if let Some(stats) = self.associations.remove(&old_key) {
                merge_stats(self.associations.entry(new_key).or_default(), stats);
            }
        }
    }

    fn prune(&mut self, now_ms: i64, config: &RankerConfig) {
        let total = self.usage.len().saturating_add(self.associations.len());
        if total <= MAX_LEARNED_RECORDS {
            return;
        }
        let remove = total - MAX_LEARNED_RECORDS;
        let mut weakest = self
            .usage
            .iter()
            .map(|(path, stats)| {
                (
                    stats.strength_milli(now_ms, config),
                    stats.last_ms,
                    path.clone(),
                    String::new(),
                    LearnedKey::Usage(path.clone()),
                )
            })
            .chain(self.associations.iter().map(|(key, stats)| {
                (
                    stats.strength_milli(now_ms, config),
                    stats.last_ms,
                    key.path.clone(),
                    key.query.clone(),
                    LearnedKey::Association(key.clone()),
                )
            }))
            .collect::<Vec<_>>();
        weakest.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
        });
        for (_, _, _, _, key) in weakest.into_iter().take(remove) {
            match key {
                LearnedKey::Usage(path) => {
                    self.usage.remove(&path);
                }
                LearnedKey::Association(key) => {
                    self.associations.remove(&key);
                }
            }
        }
    }
}

/// Query-specific score contributions and the learned paths injected into the Working Set.
#[derive(Clone, Default)]
pub struct MemoryEvidence {
    by_path: HashMap<String, LearnedStrength>,
}

#[derive(Clone, Copy, Default)]
struct LearnedStrength {
    memory_milli: i64,
    usage_milli: i64,
}

impl MemoryEvidence {
    pub fn empty() -> &'static Self {
        use std::sync::OnceLock;
        static EMPTY: OnceLock<MemoryEvidence> = OnceLock::new();
        EMPTY.get_or_init(MemoryEvidence::default)
    }

    pub fn boost(&self, path: &str, config: &RankerConfig) -> i64 {
        let (memory, usage) = self.contributions(path, config);
        memory.saturating_add(usage)
    }

    pub fn contributions(&self, path: &str, config: &RankerConfig) -> (i64, i64) {
        let strength = self.by_path.get(path).copied().unwrap_or_default();
        let memory = strength
            .memory_milli
            .saturating_mul(config.search_memory.max)
            .saturating_div(1_000);
        let usage = strength
            .usage_milli
            .saturating_mul(config.search_memory.usage_max)
            .saturating_div(1_000);
        (memory, usage)
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.by_path.keys().map(String::as_str)
    }
}

#[cfg(test)]
impl MemoryEvidence {
    pub fn from_scores(entries: &[(&str, i64)]) -> Self {
        Self {
            by_path: entries
                .iter()
                .map(|(path, strength)| {
                    (
                        (*path).to_owned(),
                        LearnedStrength {
                            memory_milli: *strength,
                            usage_milli: 0,
                        },
                    )
                })
                .collect(),
        }
    }
}

pub struct SearchMemory {
    path: PathBuf,
    file: Mutex<File>,
    aggregate: RwLock<Aggregate>,
}

impl SearchMemory {
    pub fn memory_path(app_data_dir: &Path) -> PathBuf {
        app_data_dir.join("search_memory.ndjson")
    }

    pub fn load(app_data_dir: &Path, config: &RankerConfig) -> io::Result<Self> {
        std::fs::create_dir_all(app_data_dir)?;
        let path = Self::memory_path(app_data_dir);
        let mut aggregate = Aggregate::default();
        if let Ok(existing) = File::open(&path) {
            for line in BufReader::new(existing).lines().map_while(Result::ok) {
                let Ok(record) = serde_json::from_str::<LearnedRecord>(&line) else {
                    continue;
                };
                if record.schema_version == SCHEMA_VERSION {
                    aggregate.apply(&record);
                }
            }
        }
        let newest = aggregate
            .usage
            .values()
            .chain(aggregate.associations.values())
            .map(|stats| stats.last_ms)
            .max()
            .unwrap_or(0);
        aggregate.prune(newest, config);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
            aggregate: RwLock::new(aggregate),
        })
    }

    pub fn record(
        &self,
        path: &str,
        query: Option<&str>,
        path_interpretation: bool,
        signal: SignalKind,
        timestamp_ms: i64,
        config: &RankerConfig,
    ) -> io::Result<()> {
        let normalized = query.map(normalize_query).filter(|query| !query.is_empty());
        let mut records = vec![LearnedRecord {
            schema_version: SCHEMA_VERSION,
            path: path.to_owned(),
            previous_path: None,
            query: None,
            interpretation: None,
            points: signal.points(config),
            timestamp_ms,
        }];
        if let Some(query) = normalized {
            records.push(LearnedRecord {
                schema_version: SCHEMA_VERSION,
                path: path.to_owned(),
                previous_path: None,
                query: Some(query.clone()),
                interpretation: Some(Interpretation::Ordinary),
                points: signal.points(config),
                timestamp_ms,
            });
            if path_interpretation {
                records.push(LearnedRecord {
                    schema_version: SCHEMA_VERSION,
                    path: path.to_owned(),
                    previous_path: None,
                    query: Some(query),
                    interpretation: Some(Interpretation::Path),
                    points: signal.points(config),
                    timestamp_ms,
                });
            }
        }

        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("search memory file lock is poisoned"))?;
        {
            let mut aggregate = self
                .aggregate
                .write()
                .map_err(|_| io::Error::other("search memory aggregate lock is poisoned"))?;
            for record in &records {
                aggregate.apply(record);
            }
            aggregate.prune(timestamp_ms, config);
        }
        for record in &records {
            serde_json::to_writer(&mut *file, record).map_err(io::Error::other)?;
            file.write_all(b"\n")?;
        }
        file.flush()?;
        if file.metadata()?.len() > LOG_BUDGET_BYTES {
            self.compact_locked(&mut file)?;
        }
        Ok(())
    }

    pub fn rebind(&self, previous: &str, next: &str, timestamp_ms: i64) -> io::Result<()> {
        let record = LearnedRecord {
            schema_version: SCHEMA_VERSION,
            path: next.to_owned(),
            previous_path: Some(previous.to_owned()),
            query: None,
            interpretation: None,
            points: 0,
            timestamp_ms,
        };
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("search memory file lock is poisoned"))?;
        self.aggregate
            .write()
            .map_err(|_| io::Error::other("search memory aggregate lock is poisoned"))?
            .apply(&record);
        serde_json::to_writer(&mut *file, &record).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.flush()
    }

    pub fn evidence(&self, query: &str, now_ms: i64, config: &RankerConfig) -> MemoryEvidence {
        let normalized = normalize_query(query);
        let aggregate = self.aggregate.read().expect("search memory lock poisoned");
        let mut by_path = aggregate
            .usage
            .iter()
            .filter_map(|(path, stats)| {
                let strength = stats.strength_milli(now_ms, config);
                (strength > 0).then(|| {
                    (
                        path.clone(),
                        LearnedStrength {
                            memory_milli: 0,
                            usage_milli: strength,
                        },
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        if !normalized.is_empty() {
            for (key, stats) in &aggregate.associations {
                let similarity =
                    query_similarity_milli(&normalized, &key.query, key.interpretation, config);
                if similarity == 0 {
                    continue;
                }
                let contribution = stats
                    .strength_milli(now_ms, config)
                    .saturating_mul(similarity)
                    / 1_000;
                let strength = by_path.entry(key.path.clone()).or_default();
                strength.memory_milli = strength.memory_milli.max(contribution);
            }
        }
        MemoryEvidence { by_path }
    }

    pub fn reset(&self) -> io::Result<()> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("search memory file lock is poisoned"))?;
        *self
            .aggregate
            .write()
            .map_err(|_| io::Error::other("search memory aggregate lock is poisoned"))? =
            Aggregate::default();
        *file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        file.sync_all()
    }

    fn compact_locked(&self, active: &mut File) -> io::Result<()> {
        let temp = self.path.with_extension("ndjson.tmp");
        let aggregate = self.aggregate.read().expect("search memory lock poisoned");
        let mut output = File::create(&temp)?;
        for (path, stats) in &aggregate.usage {
            write_record(
                &mut output,
                &LearnedRecord {
                    schema_version: SCHEMA_VERSION,
                    path: path.clone(),
                    previous_path: None,
                    query: None,
                    interpretation: None,
                    points: u32::try_from(stats.points).unwrap_or(u32::MAX),
                    timestamp_ms: stats.last_ms,
                },
            )?;
        }
        for (key, stats) in &aggregate.associations {
            write_record(
                &mut output,
                &LearnedRecord {
                    schema_version: SCHEMA_VERSION,
                    path: key.path.clone(),
                    previous_path: None,
                    query: Some(key.query.clone()),
                    interpretation: Some(key.interpretation),
                    points: u32::try_from(stats.points).unwrap_or(u32::MAX),
                    timestamp_ms: stats.last_ms,
                },
            )?;
        }
        output.sync_all()?;
        drop(output);
        std::fs::rename(&temp, &self.path)?;
        let replacement = OpenOptions::new().append(true).open(&self.path)?;
        *active = replacement;
        Ok(())
    }
}

fn merge_stats(target: &mut Stats, incoming: Stats) {
    target.points = target.points.saturating_add(incoming.points);
    target.last_ms = target.last_ms.max(incoming.last_ms);
}

fn rebind_path(path: &str, previous: &str, next: &str) -> Option<String> {
    if path == previous {
        return Some(next.to_owned());
    }
    let suffix = path.strip_prefix(previous)?.strip_prefix('/')?;
    Some(format!("{next}/{suffix}"))
}

fn write_record(output: &mut File, record: &LearnedRecord) -> io::Result<()> {
    serde_json::to_writer(&mut *output, record).map_err(io::Error::other)?;
    output.write_all(b"\n")
}

pub fn normalize_query(query: &str) -> String {
    query
        .nfc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_path_interpretation(query: &str, path: &Path) -> bool {
    let normalized = normalize_query(query);
    let tokens = path_tokens(&normalized);
    if tokens.len() < 2 {
        return false;
    }
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect::<Vec<_>>();
    ordered_subsequence(&tokens, &components)
}

fn query_similarity_milli(
    current: &str,
    remembered: &str,
    interpretation: Interpretation,
    config: &RankerConfig,
) -> i64 {
    if current == remembered {
        return config.search_memory.exact_similarity_milli;
    }
    let current_tokens = match interpretation {
        Interpretation::Ordinary => ordinary_tokens(current),
        Interpretation::Path => path_tokens(current),
    };
    let remembered_tokens = match interpretation {
        Interpretation::Ordinary => ordinary_tokens(remembered),
        Interpretation::Path => path_tokens(remembered),
    };
    if current_tokens == remembered_tokens {
        return config.search_memory.exact_similarity_milli;
    }
    if current.starts_with(remembered) {
        return config.search_memory.prefix_extension_similarity_milli;
    }
    if remembered.starts_with(current) {
        return config.search_memory.prefix_contraction_similarity_milli;
    }
    if one_token_added_or_removed(&current_tokens, &remembered_tokens) {
        return config.search_memory.token_delta_similarity_milli;
    }
    if one_token_edit(&current_tokens, &remembered_tokens) {
        return config.search_memory.edit_similarity_milli;
    }
    0
}

fn ordinary_tokens(query: &str) -> Vec<String> {
    let mut tokens = query
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn path_tokens(query: &str) -> Vec<String> {
    query
        .split(|character: char| character.is_whitespace() || character == '/')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn ordered_subsequence(needles: &[String], haystack: &[String]) -> bool {
    let mut next = 0usize;
    for needle in needles {
        let Some(offset) = haystack[next..]
            .iter()
            .position(|component| component.contains(needle))
        else {
            return false;
        };
        next += offset + 1;
    }
    true
}

fn one_token_added_or_removed(left: &[String], right: &[String]) -> bool {
    let (shorter, longer) = if left.len() < right.len() {
        (left, right)
    } else {
        (right, left)
    };
    if longer.len() != shorter.len() + 1 {
        return false;
    }
    let shorter = shorter.iter().collect::<HashSet<_>>();
    longer
        .iter()
        .filter(|token| !shorter.contains(token))
        .count()
        == 1
}

fn one_token_edit(left: &[String], right: &[String]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let changed = left
        .iter()
        .zip(right)
        .filter(|(left, right)| left != right)
        .collect::<Vec<_>>();
    let [(left, right)] = changed.as_slice() else {
        return false;
    };
    let length = left.chars().count().max(right.chars().count());
    let allowed = match length {
        0..=4 => 0,
        5..=8 => 1,
        _ => 2.min(length / 5),
    };
    allowed > 0 && osa_distance(left, right, allowed).is_some()
}

fn osa_distance(left: &str, right: &str, limit: usize) -> Option<usize> {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    if left.len().abs_diff(right.len()) > limit {
        return None;
    }
    let mut two_back = (0..=right.len()).collect::<Vec<_>>();
    let mut previous = two_back.clone();
    let mut current = vec![0; right.len() + 1];
    for i in 1..=left.len() {
        current[0] = i;
        let mut row_min = i;
        for j in 1..=right.len() {
            let substitution = previous[j - 1] + usize::from(left[i - 1] != right[j - 1]);
            let mut value = (previous[j] + 1).min(current[j - 1] + 1).min(substitution);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                value = value.min(two_back[j - 2] + 1);
            }
            current[j] = value;
            row_min = row_min.min(value);
        }
        if row_min > limit {
            return None;
        }
        std::mem::swap(&mut two_back, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    (previous[right.len()] <= limit).then_some(previous[right.len()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "beeline_search_memory_{}_{}",
                std::process::id(),
                unique
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn normalizes_and_transfers_only_one_supported_change() {
        let config = RankerConfig::default();
        assert_eq!(normalize_query("  Work   WIP  "), "work wip");
        assert_eq!(
            query_similarity_milli("wip work", "work wip", Interpretation::Ordinary, &config),
            1_000
        );
        assert_eq!(
            query_similarity_milli("work wip", "wip work", Interpretation::Path, &config),
            0
        );
        assert_eq!(
            query_similarity_milli(
                "methodology",
                "metodology",
                Interpretation::Ordinary,
                &config
            ),
            650
        );
        assert_eq!(
            query_similarity_milli("method", "metod", Interpretation::Ordinary, &config),
            650
        );
        assert_eq!(
            query_similarity_milli("file", "fiel", Interpretation::Ordinary, &config),
            0
        );
    }

    #[test]
    fn path_interpretation_accepts_omitted_components_in_order() {
        assert!(is_path_interpretation(
            "work wip",
            Path::new("/Users/kiri/work/archive/wip")
        ));
        assert!(!is_path_interpretation(
            "wip work",
            Path::new("/Users/kiri/work/archive/wip")
        ));
    }

    #[test]
    fn persists_scores_and_reset_preserves_a_valid_empty_store() {
        let dir = TempDir::new();
        let path = "/Users/kiri/work/wip";
        let config = RankerConfig::default();
        {
            let memory = SearchMemory::load(&dir.0, &config).expect("load");
            memory
                .record(
                    path,
                    Some("work wip"),
                    true,
                    SignalKind::CompletedAction,
                    1_000,
                    &config,
                )
                .expect("record");
            let evidence = memory.evidence("work/wip", 1_000, &config);
            assert!(evidence.boost(path, &config) > 0);
        }
        let memory = SearchMemory::load(&dir.0, &config).expect("reload");
        assert!(
            memory
                .evidence("work wip", 1_000, &config)
                .boost(path, &config)
                > 0
        );
        memory.reset().expect("reset");
        assert_eq!(
            memory
                .evidence("work wip", 1_000, &config)
                .boost(path, &config),
            0
        );
        drop(memory);
        let empty = SearchMemory::load(&dir.0, &config).expect("reload empty");
        assert_eq!(
            empty
                .evidence("work wip", 1_000, &config)
                .boost(path, &config),
            0
        );
    }

    #[test]
    fn stronger_signals_accumulate_monotonically_and_age() {
        let dir = TempDir::new();
        let config = RankerConfig::default();
        let memory = SearchMemory::load(&dir.0, &config).expect("load");
        let path = "/tmp/report";
        memory
            .record(
                path,
                Some("report"),
                false,
                SignalKind::ActionMenu,
                1_000,
                &config,
            )
            .expect("weak");
        let weak = memory
            .evidence("report", 1_000, &config)
            .boost(path, &config);
        memory
            .record(
                path,
                Some("report"),
                false,
                SignalKind::QuickLook,
                2_000,
                &config,
            )
            .expect("medium");
        let medium = memory
            .evidence("report", 2_000, &config)
            .boost(path, &config);
        memory
            .record(
                path,
                Some("report"),
                false,
                SignalKind::CompletedAction,
                3_000,
                &config,
            )
            .expect("strong");
        let strong = memory
            .evidence("report", 3_000, &config)
            .boost(path, &config);
        assert!(weak < medium && medium < strong);
        let aged = memory
            .evidence(
                "report",
                3_000 + 2 * config.search_memory.half_life_days * DAY_MS,
                &config,
            )
            .boost(path, &config);
        assert!(aged < strong);
    }

    #[test]
    fn rebind_moves_item_and_descendant_evidence_and_survives_reload() {
        let dir = TempDir::new();
        let old = "/tmp/project";
        let old_child = "/tmp/project/docs/report.md";
        let next = "/tmp/project-renamed";
        let config = RankerConfig::default();
        {
            let memory = SearchMemory::load(&dir.0, &config).expect("load");
            memory
                .record(
                    old_child,
                    Some("report"),
                    false,
                    SignalKind::CompletedAction,
                    1_000,
                    &config,
                )
                .expect("record");
            memory.rebind(old, next, 2_000).expect("rebind");
            assert_eq!(
                memory
                    .evidence("report", 2_000, &config)
                    .boost(old_child, &config),
                0
            );
            assert!(
                memory
                    .evidence("report", 2_000, &config)
                    .boost("/tmp/project-renamed/docs/report.md", &config)
                    > 0
            );
        }
        let reloaded = SearchMemory::load(&dir.0, &config).expect("reload");
        assert!(
            reloaded
                .evidence("report", 2_000, &config)
                .boost("/tmp/project-renamed/docs/report.md", &config)
                > 0
        );
    }
}
