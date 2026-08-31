mod name_index;

// The production ranker raises its worker threads to user-initiated QoS. The prototype
// runs from a foreground shell and only needs the same callable seam.
mod qos {
    pub fn set_user_initiated_qos() {}
}

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use name_index::{LiveIndex, SearchMeasurement};
use serde::Serialize;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

const THRESHOLDS: [usize; 3] = [4, 5, 6];

#[derive(Clone)]
struct Case {
    id: String,
    category: String,
    query: String,
    target: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    ExactPath,
    Alias,
    WorkingSet,
    Global,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThresholdDecision {
    threshold: usize,
    scope: Scope,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseReport {
    id: String,
    category: String,
    query: String,
    significant_characters: usize,
    target: Option<String>,
    target_in_working_set: Option<bool>,
    decisions: Vec<ThresholdDecision>,
    working_set: SearchMeasurement,
    global: Option<SearchMeasurement>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkingSetReport {
    current_location: String,
    current_location_items: usize,
    historical_paths: usize,
    resolved_historical_items: usize,
    unresolved_historical_paths: usize,
    total_items: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HotUpdateStep {
    threshold: usize,
    scope: Scope,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    index_path: String,
    index_items: usize,
    index_directories: usize,
    index_load_ms: f64,
    working_set: WorkingSetReport,
    count_contract: BTreeMap<String, usize>,
    aliases: Vec<String>,
    hot_update_query: String,
    hot_update: Vec<HotUpdateStep>,
    cases: Vec<CaseReport>,
}

struct Config {
    index: PathBuf,
    root: PathBuf,
    current: PathBuf,
    recents: PathBuf,
    journal: PathBuf,
    pinned: Option<PathBuf>,
    cases: PathBuf,
    aliases: Vec<(String, String)>,
}

fn main() {
    let config = parse_args();
    self_check();

    let load_started = Instant::now();
    let index = LiveIndex::load(&config.index, &config.root, &config.aliases);
    let index_load_ms = load_started.elapsed().as_secs_f64() * 1000.0;

    let mut historical_paths = read_recents(&config.recents);
    historical_paths.extend(read_journal(&config.journal));
    if let Some(pinned) = &config.pinned {
        historical_paths.extend(read_pinned(pinned));
    }
    let historical_paths: BTreeSet<_> = historical_paths.into_iter().collect();
    let (historical_slots, unresolved_historical_paths) =
        index.slots_for_paths(historical_paths.iter().map(PathBuf::as_path));
    let current_slots = index.direct_slots(&config.current);
    let working_slots = LiveIndex::merge_slots([current_slots.clone(), historical_slots.clone()]);

    let cases = read_cases(&config.cases);
    let alias_words: BTreeSet<_> = config
        .aliases
        .iter()
        .map(|(word, _)| normalize(word))
        .collect();
    let case_reports = cases
        .into_iter()
        .map(|case| {
            let significant_characters = significant_len(&case.query);
            let decisions = THRESHOLDS
                .into_iter()
                .map(|threshold| ThresholdDecision {
                    threshold,
                    scope: choose_scope(&case.query, threshold, &alias_words),
                })
                .collect::<Vec<_>>();
            let target_in_working_set = case
                .target
                .as_deref()
                .map(|target| index.contains_path(&working_slots, target));
            let working_set =
                index.measure(&case.query, Some(&working_slots), case.target.as_deref(), 5);
            let global = decisions
                .iter()
                .any(|decision| decision.scope == Scope::Global)
                .then(|| index.measure(&case.query, None, case.target.as_deref(), 5));
            CaseReport {
                id: case.id,
                category: case.category,
                query: case.query,
                significant_characters,
                target: case.target.map(|path| path.to_string_lossy().into_owned()),
                target_in_working_set,
                decisions,
                working_set,
                global,
            }
        })
        .collect();

    let hot_update_query = "conte".to_owned();
    let hot_update = [6, 5, 4, 6]
        .into_iter()
        .map(|threshold| HotUpdateStep {
            threshold,
            scope: choose_scope(&hot_update_query, threshold, &alias_words),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        hot_update.iter().map(|step| step.scope).collect::<Vec<_>>(),
        [
            Scope::WorkingSet,
            Scope::Global,
            Scope::Global,
            Scope::WorkingSet
        ]
    );

    let report = Report {
        index_path: config.index.to_string_lossy().into_owned(),
        index_items: index.item_count(),
        index_directories: index.directory_count(),
        index_load_ms,
        working_set: WorkingSetReport {
            current_location: config.current.to_string_lossy().into_owned(),
            current_location_items: current_slots.len(),
            historical_paths: historical_paths.len(),
            resolved_historical_items: historical_slots.len(),
            unresolved_historical_paths,
            total_items: working_slots.len(),
        },
        count_contract: count_contract(),
        aliases: config
            .aliases
            .iter()
            .map(|(word, path)| format!("{word}={path}"))
            .collect(),
        hot_update_query,
        hot_update,
        cases: case_reports,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize report")
    );
}

fn choose_scope(query: &str, threshold: usize, aliases: &BTreeSet<String>) -> Scope {
    let normalized = normalize(query);
    if is_existing_explicit_path(query) {
        Scope::ExactPath
    } else if aliases.contains(&normalized) {
        Scope::Alias
    } else if significant_len(query) >= threshold {
        Scope::Global
    } else {
        Scope::WorkingSet
    }
}

fn significant_len(query: &str) -> usize {
    query
        .nfc()
        .filter(|character| character.is_alphanumeric())
        .count()
}

fn normalize(query: &str) -> String {
    query
        .nfc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_existing_explicit_path(query: &str) -> bool {
    let trimmed = query.trim();
    let expanded = if trimmed == "~" {
        env::var_os("HOME").map(PathBuf::from)
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(rest))
    } else if trimmed.starts_with('/') {
        Some(PathBuf::from(trimmed))
    } else {
        None
    };
    expanded.is_some_and(|path| path.exists())
}

fn count_contract() -> BTreeMap<String, usize> {
    [
        ("work wip", 7),
        ("work/wip", 7),
        ("2026", 4),
        ("план то", 6),
        ("./a1-b2", 4),
        ("../", 0),
        ("~/.config", 6),
    ]
    .into_iter()
    .map(|(query, expected)| {
        assert_eq!(significant_len(query), expected, "count {query:?}");
        (query.to_owned(), expected)
    })
    .collect()
}

fn self_check() {
    let aliases = BTreeSet::from(["bee".to_owned()]);
    assert_eq!(choose_scope("hand", 4, &aliases), Scope::Global);
    assert_eq!(choose_scope("hand", 5, &aliases), Scope::WorkingSet);
    assert_eq!(choose_scope("bee", 6, &aliases), Scope::Alias);
    assert_eq!(choose_scope("/Users", 99, &aliases), Scope::ExactPath);
    let _ = count_contract();
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

fn read_cases(path: &Path) -> Vec<Case> {
    fs::read_to_string(path)
        .expect("read query cases")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let columns = line.split('\t').collect::<Vec<_>>();
            assert_eq!(
                columns.len(),
                4,
                "case must have four tab-separated columns"
            );
            Case {
                id: columns[0].to_owned(),
                category: columns[1].to_owned(),
                query: columns[2].to_owned(),
                target: (columns[3] != "-").then(|| PathBuf::from(columns[3])),
            }
        })
        .collect()
}

fn parse_args() -> Config {
    let mut values = env::args().skip(1);
    let mut keyed = BTreeMap::<String, String>::new();
    let mut aliases = Vec::new();
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .unwrap_or_else(|| panic!("missing value for {flag}"));
        if flag == "--alias" {
            let (word, path) = value
                .split_once('=')
                .expect("--alias must be word=/absolute/path");
            aliases.push((word.to_owned(), path.to_owned()));
        } else {
            keyed.insert(flag, value);
        }
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
        aliases,
    }
}
