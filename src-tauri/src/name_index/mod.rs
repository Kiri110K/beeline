//! The Name Index (SPEC §6): an application-owned index of item names and paths — no
//! content — that answers Search Queries over the home directory. Mounted-volume
//! support is a later chunk; the index is keyed by a root path so that seam is clean.
//!
//! Concurrency: the index lives behind `Arc<RwLock<IndexData>>`. Searches take the read
//! lock and never block each other; the crawl, incremental apply, diff-rescan, and Junk
//! drain take short write locks (the initial crawl never holds it across IO — see
//! `crawl.rs`). Background work runs on plain `std::thread`s lowered to background QoS.

mod alias;
mod crawl;
mod junk;
mod model;
mod persist;
mod query;
mod visit_journal;
mod watcher;

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager, State};

use crate::telemetry::Telemetry;
use alias::AliasDictionary;
use junk::JunkPatterns;
use model::IndexData;
use query::{RankContext, SearchHit};
use visit_journal::{VisitJournal, VisitKind};

/// The volume key for the home root. Mounted volumes get their own keys later.
const HOME_VOLUME_KEY: &str = "home";

/// The built-in Junk seed list (SPEC §6), re-exported so the settings store can seed a
/// fresh install's editable list from the same source (SPEC §12).
pub(crate) fn builtin_junk_names() -> Vec<String> {
    junk::default_names()
}

/// A swappable Junk-patterns handle. Search, the watcher, and the Junk drain each take a
/// cheap snapshot (`.read().clone()`); `apply_settings` swaps the inner `Arc` so a Settings
/// change reaches every future classification without re-crawling (Junk refreshes lazily,
/// SPEC §6, §10).
type SharedJunk = Arc<RwLock<Arc<JunkPatterns>>>;

/// One cached exhaustive scan, reused when the next query strictly extends this one under
/// the same index revision (typing-extension reuse, SPEC §10). Storing entry *slots* (not
/// scored candidates) keeps it tiny and lets the extended query re-score from scratch.
#[derive(Clone)]
struct ReuseCache {
    /// The trimmed, lowercased query these entries matched.
    query_lower: String,
    /// Index revision the slots were collected at; a mismatch means the index changed and
    /// the slots may be stale, so reuse is skipped.
    revision: u64,
    /// Slot indices into `IndexData::entries` of the full (exhaustive) match set.
    entries: Vec<u32>,
}

/// Managed Tauri state holding the live index and everything needed to search, drain,
/// and persist it.
pub struct NameIndex {
    data: Arc<RwLock<IndexData>>,
    junk: SharedJunk,
    root: PathBuf,
    index_file: PathBuf,
    /// Local, ranking-only record of visits (SPEC §6, §11).
    journal: Arc<VisitJournal>,
    /// The Alias Dictionary (SPEC §6), swapped in place when Settings change so new
    /// searches rank against the current aliases.
    aliases: RwLock<Arc<AliasDictionary>>,
    /// Monotonic query counter (SPEC §10 cancel-on-newer). `search_name_index` bumps it on
    /// entry; each running scan carries its own stamp and aborts once a newer query moves the
    /// counter past it.
    generation: Arc<AtomicU64>,
    /// The single previous exhaustive scan (the app has one search stream, SPEC §5), so a
    /// strictly-extending keystroke rescans just that match set instead of the whole index.
    reuse: Arc<Mutex<Option<ReuseCache>>>,
}

impl NameIndex {
    /// Build the index and start background work. Called from `setup()` after the
    /// window is created; it only spawns threads and returns immediately, so window
    /// show is never delayed. On start it loads any persisted index and diff-rescans,
    /// otherwise it runs the one-time full crawl; either way it persists on completion.
    pub fn init(app: &AppHandle) -> Result<Self, String> {
        let root = app
            .path()
            .home_dir()
            .map_err(|error| format!("failed to resolve home dir: {error}"))?;
        // Canonicalize so paths match FSEvents, which reports through symlinks (e.g.
        // `/var` → `/private/var`). Without this, `strip_prefix(root)` would silently
        // drop every incremental event under a symlinked root.
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let app_data_dir = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("failed to resolve app data dir: {error}"))?;
        let index_file = persist::index_path(&app_data_dir, HOME_VOLUME_KEY);

        // Seed the Junk classifier and Alias Dictionary from the persisted settings store
        // (SPEC §12): a fresh install's defaults carry the built-in Junk list and any
        // migrated aliases, so both consumers start from the user's configuration.
        let (settings, _) = crate::settings::read(&app_data_dir);
        let junk: SharedJunk = Arc::new(RwLock::new(Arc::new(JunkPatterns::from_names(
            settings.junk_patterns,
        ))));
        let aliases = RwLock::new(Arc::new(AliasDictionary::from_pairs(
            settings
                .aliases
                .into_iter()
                .map(|entry| (entry.word, entry.path)),
            &root,
        )));

        let loaded = persist::load(&index_file, &root);
        let had_persisted = loaded.is_some();
        let data = Arc::new(RwLock::new(
            loaded.unwrap_or_else(|| IndexData::new(root.clone())),
        ));

        {
            let data = data.clone();
            let junk = junk.read().expect("junk lock poisoned").clone();
            let root = root.clone();
            let app = app.clone();
            let index_file = index_file.clone();
            thread::spawn(move || {
                crawl::set_crawl_qos();
                if had_persisted {
                    let entries = data.read().expect("name index lock poisoned").len();
                    record(&app, "index_loaded", json!({ "entries": entries }));
                    crawl::diff_rescan(&data, root, &junk, Some(&app));
                } else {
                    crawl::initial_crawl(&data, root, &junk, Some(&app));
                }
                if let Err(error) = persist::save(&data, &index_file) {
                    eprintln!("name index persist after crawl failed: {error}");
                }
            });
        }

        watcher::spawn(data.clone(), root.clone(), junk.clone());

        let journal = Arc::new(
            VisitJournal::load(&app_data_dir)
                .map_err(|error| format!("failed to load visit journal: {error}"))?,
        );

        Ok(Self {
            data,
            junk,
            root,
            index_file,
            journal,
            aliases,
            generation: Arc::new(AtomicU64::new(0)),
            reuse: Arc::new(Mutex::new(None)),
        })
    }

    /// A cheap snapshot of the current Junk patterns (SPEC §6), for a search or drain.
    fn junk_snapshot(&self) -> Arc<JunkPatterns> {
        self.junk.read().expect("junk lock poisoned").clone()
    }

    /// A cheap snapshot of the current Alias Dictionary (SPEC §6), for a search.
    fn aliases_snapshot(&self) -> Arc<AliasDictionary> {
        self.aliases.read().expect("aliases lock poisoned").clone()
    }

    /// Swap in Junk patterns and aliases from a Settings change (SPEC §12). Future
    /// classifications and searches read the new values immediately; already-indexed items
    /// keep their tier until the tree is next rescanned (Junk refreshes lazily, SPEC §6).
    pub fn apply_settings(&self, junk_patterns: Vec<String>, aliases: Vec<(String, String)>) {
        let next_junk = Arc::new(JunkPatterns::from_names(junk_patterns));
        let next_aliases = Arc::new(AliasDictionary::from_pairs(aliases, &self.root));
        *self.junk.write().expect("junk lock poisoned") = next_junk;
        *self.aliases.write().expect("aliases lock poisoned") = next_aliases;
    }

    /// Persist the current index atomically. Called on graceful shutdown (never
    /// periodically), in addition to the post-crawl write.
    pub fn persist(&self) {
        if let Err(error) = persist::save(&self.data, &self.index_file) {
            eprintln!("name index persist on shutdown failed: {error}");
        }
    }
}

fn record(app: &AppHandle, event: &str, fields: serde_json::Value) {
    if let Err(error) = app.state::<Telemetry>().record(event, fields) {
        eprintln!("telemetry event {event} failed: {error}");
    }
}

/// Wall-clock milliseconds since the Unix epoch, for stamping visits.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The search response: the ranked hits plus the index generation they were computed
/// against, so the UI can later reconcile progressive results (SPEC §6).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub revision: u64,
    /// The scan reused the previous query's candidate set instead of walking the whole
    /// index (typing-extension reuse); passed through to sampled search telemetry (SPEC §10).
    pub reused: bool,
    /// How many entries the scan scored — small for a reuse scan, up to the whole index for
    /// a cold full scan; passed through to sampled search telemetry.
    pub scanned: usize,
    pub hits: Vec<SearchHit>,
}

/// One executed search: the ranked outcome plus the revision it ran against and whether it
/// reused a cached candidate set.
struct Executed {
    revision: u64,
    reused: bool,
    outcome: query::SearchOutcome,
}

/// Whether `next_lower` is safe to answer by rescanning `prev_lower`'s exhaustive candidate
/// set. It must strictly extend `prev_lower` — which guarantees the match set only shrinks
/// (prefix/substring/exact matching is monotone under extension) — *and* introduce no new
/// `/`: a new slash re-shapes a plain query into a path-shaped one (or adds a path segment),
/// changing the scan's scope entirely, so we fall back to a full scan. A leading `~` cannot
/// appear via extension (the prefix is shared), so path-shapedness is otherwise preserved.
/// Layout correction survives too: the corrected variant of the extended query extends the
/// corrected variant of the prefix (the key map is per-character), so every corrected match
/// of `next_lower` is already in `prev_lower`'s cached set.
fn can_extend(prev_lower: &str, next_lower: &str) -> bool {
    !prev_lower.is_empty()
        && next_lower.len() > prev_lower.len()
        && next_lower.starts_with(prev_lower)
        && !next_lower[prev_lower.len()..].contains('/')
}

/// Shared search core: drain targeted Junk, then scan under one read lock, optionally
/// reusing `reuse`'s candidate set (when the query extends it under the same revision) and
/// honoring `cancel`.
#[allow(clippy::too_many_arguments)]
fn execute_search(
    data: &Arc<RwLock<IndexData>>,
    junk: &Arc<JunkPatterns>,
    root: &Path,
    journal: &VisitJournal,
    aliases: &AliasDictionary,
    query: &str,
    limit: usize,
    reuse: Option<&ReuseCache>,
    cancel: Option<&query::Cancel>,
) -> Executed {
    let query_lower = query.trim().to_lowercase();
    if junk.query_targets_junk(&query_lower) {
        crawl::drain_junk_dirty(data, root, junk);
    }
    let aggregate = journal.aggregate();
    let ctx = RankContext {
        journal: &aggregate,
        aliases,
    };
    let index = data.read().expect("name index lock poisoned");
    let revision = index.revision;
    // Reuse only when the cache was built against this exact index revision (slots still
    // valid) and the query strictly extends the cached one without re-shaping its scope.
    let reuse_slots = reuse.and_then(|cache| {
        (cache.revision == revision && can_extend(&cache.query_lower, &query_lower))
            .then_some(cache.entries.as_slice())
    });
    let reused = reuse_slots.is_some();
    let outcome = query::run(&index, &ctx, query, limit, reuse_slots, cancel);
    Executed {
        revision,
        reused,
        outcome,
    }
}

/// Run a search with no cancellation or reuse (the direct path the tests exercise).
/// Production searches go through `search_name_index`, which adds stale-cancellation and
/// typing-extension reuse on top of the same `execute_search` core.
#[cfg(test)]
fn run_search(
    data: &Arc<RwLock<IndexData>>,
    junk: &Arc<JunkPatterns>,
    root: &Path,
    journal: &VisitJournal,
    aliases: &AliasDictionary,
    query: &str,
    limit: usize,
) -> SearchResponse {
    let executed = execute_search(data, junk, root, journal, aliases, query, limit, None, None);
    SearchResponse {
        revision: executed.revision,
        reused: executed.reused,
        scanned: executed.outcome.scanned,
        hits: executed.outcome.hits,
    }
}

/// Query the Name Index. Runs off the async runtime's core threads (like `list_location`)
/// so the read scan and any Junk drain never block IPC. Every call bumps the query
/// generation so any still-running older scan aborts (SPEC §10 cancel-on-newer), and a
/// strictly-extending query reuses the previous exhaustive scan's candidate set.
#[tauri::command]
pub async fn search_name_index(
    query: String,
    limit: u32,
    state: State<'_, NameIndex>,
) -> Result<SearchResponse, String> {
    // Bump on entry so any older scan still in flight sees itself superseded and aborts.
    let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let data = state.data.clone();
    let junk = state.junk_snapshot();
    let root = state.root.clone();
    let journal = state.journal.clone();
    let aliases = state.aliases_snapshot();
    let generation = state.generation.clone();
    let reuse = state.reuse.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let cached = reuse.lock().expect("reuse cache lock poisoned").clone();
        let cancel = query::Cancel::new(&generation, my_gen);
        let executed = execute_search(
            &data,
            &junk,
            &root,
            &journal,
            &aliases,
            &query,
            limit as usize,
            cached.as_ref(),
            Some(&cancel),
        );
        // Superseded: return an explicit error. The frontend's per-Tab seq-guard already
        // drops it — a newer query on the single search stream is by definition in flight —
        // so it never reaches the UI (SPEC §6 progressive/stale handling).
        if executed.outcome.aborted {
            return Err("search superseded".to_owned());
        }
        // Cache the match set only when the scan was exhaustive; a capped scan (or one whose
        // reuse is no longer valid) clears the slot so the next query starts from a full scan.
        {
            let mut slot = reuse.lock().expect("reuse cache lock poisoned");
            if executed.outcome.exhaustive {
                *slot = Some(ReuseCache {
                    query_lower: query.trim().to_lowercase(),
                    revision: executed.revision,
                    entries: executed.outcome.candidate_entries,
                });
            } else {
                *slot = None;
            }
        }
        Ok(SearchResponse {
            revision: executed.revision,
            reused: executed.reused,
            scanned: executed.outcome.scanned,
            hits: executed.outcome.hits,
        })
    })
    .await
    .map_err(|_| "name index search task failed".to_owned())?
}

/// Record a visit into the Visit Journal (SPEC §6). The frontend wires this into
/// navigation and open actions in a later chunk; it records liberally.
#[tauri::command]
pub async fn record_visit(
    path: String,
    kind: String,
    state: State<'_, NameIndex>,
) -> Result<(), String> {
    let kind = VisitKind::parse(&kind).ok_or_else(|| format!("unknown visit kind: {kind}"))?;
    let timestamp = now_ms();
    let journal = state.journal.clone();
    tauri::async_runtime::spawn_blocking(move || journal.record(&path, kind, timestamp))
        .await
        .map_err(|_| "record visit task failed".to_owned())?
        .map_err(|error| format!("failed to record visit: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique temp directory removed on drop (no `tempfile` dependency available).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("beeline_ni_{}_{}", std::process::id(), unique));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, b"x").expect("write file");
    }

    fn new_index(root: &Path) -> Arc<RwLock<IndexData>> {
        Arc::new(RwLock::new(IndexData::new(root.to_path_buf())))
    }

    fn tier_of(shared: &Arc<RwLock<IndexData>>, name: &str) -> Option<&'static str> {
        let index = shared.read().unwrap();
        query::search(&index, &RankContext::empty(), name, 50)
            .into_iter()
            .find(|hit| hit.name == name)
            .map(|hit| hit.tier)
    }

    /// An empty, tempdir-backed journal for tests that route through `run_search`.
    fn empty_journal() -> (TempDir, VisitJournal) {
        let dir = TempDir::new();
        let journal = VisitJournal::load(dir.path()).expect("load journal");
        (dir, journal)
    }

    fn build_sample_tree(root: &Path) {
        touch(&root.join("alpha.txt"));
        touch(&root.join("sub/beta.txt"));
        touch(&root.join("sub/.hidden/secret.txt"));
        touch(&root.join("node_modules/pkg/index.js"));
        touch(&root.join(".git/config"));
    }

    #[test]
    fn crawl_classifies_nested_hidden_and_junk() {
        let dir = TempDir::new();
        build_sample_tree(dir.path());
        let junk = JunkPatterns::default();
        let shared = new_index(dir.path());

        crawl::initial_crawl(&shared, dir.path().to_path_buf(), &junk, None);

        assert_eq!(tier_of(&shared, "alpha.txt"), Some("normal"));
        assert_eq!(tier_of(&shared, "beta.txt"), Some("normal"));
        assert_eq!(tier_of(&shared, "secret.txt"), Some("hidden"));
        assert_eq!(tier_of(&shared, ".hidden"), Some("hidden"));
        assert_eq!(tier_of(&shared, "index.js"), Some("junk"));
        assert_eq!(tier_of(&shared, "config"), Some("junk"));
        // node_modules itself is Junk (junk-named component).
        assert_eq!(tier_of(&shared, "node_modules"), Some("junk"));
    }

    #[test]
    fn incremental_add_and_remove() {
        let dir = TempDir::new();
        build_sample_tree(dir.path());
        let junk = JunkPatterns::default();
        let shared = new_index(dir.path());
        crawl::initial_crawl(&shared, dir.path().to_path_buf(), &junk, None);

        // Add a file.
        let added = dir.path().join("added.txt");
        touch(&added);
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &added, &junk);
        }
        assert_eq!(tier_of(&shared, "added.txt"), Some("normal"));

        // Add a whole subtree at once (directory moved in).
        let moved_file = dir.path().join("moved/inner/deep.txt");
        touch(&moved_file);
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &dir.path().join("moved"), &junk);
        }
        assert_eq!(tier_of(&shared, "deep.txt"), Some("normal"));

        // Remove a file.
        fs::remove_file(&added).unwrap();
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &added, &junk);
        }
        assert_eq!(tier_of(&shared, "added.txt"), None);
    }

    #[test]
    fn persistence_round_trip() {
        let dir = TempDir::new();
        build_sample_tree(dir.path());
        let junk = JunkPatterns::default();
        let shared = new_index(dir.path());
        crawl::initial_crawl(&shared, dir.path().to_path_buf(), &junk, None);
        let before = shared.read().unwrap().len();

        let file = dir.path().join("saved.idx");
        persist::save(&shared, &file).expect("save");
        let loaded = persist::load(&file, dir.path()).expect("load");
        assert_eq!(loaded.len(), before);

        let reloaded = Arc::new(RwLock::new(loaded));
        assert_eq!(tier_of(&reloaded, "index.js"), Some("junk"));
        assert_eq!(tier_of(&reloaded, "secret.txt"), Some("hidden"));

        // A root mismatch is rejected (per-volume identity).
        assert!(persist::load(&file, Path::new("/nonexistent/root")).is_none());
    }

    #[test]
    fn query_ordering_and_limit() {
        let dir = TempDir::new();
        let root = dir.path();
        let shared = new_index(root);
        {
            let mut index = shared.write().unwrap();
            // Prefix match at root, prefix match nested (longer path), substring match.
            index.add_file(0, "readme.md", model::Tier::Normal);
            let sub = index.add_dir(0, "a", model::Tier::Normal, 0);
            index.add_file(sub, "readme.md", model::Tier::Normal);
            index.add_file(0, "myreadme.md", model::Tier::Normal);
        }

        let index = shared.read().unwrap();
        let hits = query::search(&index, &RankContext::empty(), "readme", 10);
        let paths: Vec<&str> = hits.iter().map(|hit| hit.path.as_str()).collect();
        // Prefix matches first (shorter path before longer), then the substring match.
        assert_eq!(
            paths,
            vec![
                root.join("readme.md").to_string_lossy().as_ref(),
                root.join("a/readme.md").to_string_lossy().as_ref(),
                root.join("myreadme.md").to_string_lossy().as_ref(),
            ]
        );

        // The limit is honored.
        let limited = query::search(&index, &RankContext::empty(), "readme", 1);
        assert_eq!(limited.len(), 1);
        assert_eq!(
            limited[0].path,
            root.join("readme.md").to_string_lossy().as_ref()
        );

        // Case-insensitive.
        assert_eq!(
            query::search(&index, &RankContext::empty(), "README", 10).len(),
            3
        );
    }

    #[test]
    fn junk_dirty_queue_drains_on_targeting_query() {
        let dir = TempDir::new();
        let root = dir.path();
        touch(&root.join("node_modules/pkg/index.js"));
        let junk = JunkPatterns::default();
        let shared = new_index(root);
        crawl::initial_crawl(&shared, root.to_path_buf(), &junk, None);

        // A new file appears inside Junk after the crawl.
        let new_file = root.join("node_modules/pkg/added.js");
        touch(&new_file);
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &new_file, &junk);
        }

        // Lazy: it is not indexed yet, only the containing dir is marked dirty.
        assert_eq!(tier_of(&shared, "added.js"), None);
        assert!(!shared.read().unwrap().junk_dirty.is_empty());

        // A targeting query (contains a Junk name) drains the dirty Junk dirs.
        let (_journal_dir, journal) = empty_journal();
        let response = run_search(
            &shared,
            &Arc::new(JunkPatterns::default()),
            root,
            &journal,
            &AliasDictionary::empty(),
            "node_modules",
            50,
        );
        assert!(response.hits.iter().any(|hit| hit.name == "node_modules"));
        // The file added into Junk after the crawl is now indexed.
        assert_eq!(tier_of(&shared, "added.js"), Some("junk"));
        assert!(shared.read().unwrap().junk_dirty.is_empty());
    }

    // FSEvents delivery is environment- and timing-dependent, so the real watcher is
    // only smoke-covered here and ignored by default. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn fsevents_watcher_smoke() {
        use std::time::Duration;

        let dir = TempDir::new();
        // Canonicalize: macOS temp dirs live under the `/var` → `/private/var` symlink,
        // and FSEvents reports canonical paths (matches the init-time canonicalization).
        let root = fs::canonicalize(dir.path()).unwrap();
        let junk = Arc::new(RwLock::new(Arc::new(JunkPatterns::default())));
        let shared = new_index(&root);
        watcher::spawn(shared.clone(), root.clone(), junk);

        thread::sleep(Duration::from_millis(300));
        touch(&root.join("live.txt"));
        thread::sleep(Duration::from_millis(1500));

        assert_eq!(tier_of(&shared, "live.txt"), Some("normal"));
    }

    #[test]
    fn existing_typed_path_ranks_first() {
        // Guarantee (a): an existing absolute path typed as the query ranks its target
        // first, deterministically. Uses the real filesystem (path-shaped queries only).
        let dir = TempDir::new();
        let root = fs::canonicalize(dir.path()).unwrap();
        touch(&root.join("deep/target.txt"));
        // A decoy that also matches "target.txt" by name but is not the typed path.
        touch(&root.join("other/target.txt"));

        let junk = JunkPatterns::default();
        let shared = new_index(&root);
        crawl::initial_crawl(&shared, root.clone(), &junk, None);

        let (_journal_dir, journal) = empty_journal();
        let typed = root.join("deep/target.txt");
        let response = run_search(
            &shared,
            &Arc::new(JunkPatterns::default()),
            &root,
            &journal,
            &AliasDictionary::empty(),
            &typed.to_string_lossy(),
            50,
        );
        assert_eq!(response.hits[0].path, typed.to_string_lossy());
    }

    #[test]
    fn search_response_carries_revision() {
        let dir = TempDir::new();
        let shared = new_index(dir.path());
        {
            let mut index = shared.write().unwrap();
            index.add_file(0, "alpha.txt", model::Tier::Normal);
        }
        let (_journal_dir, journal) = empty_journal();
        let response = run_search(
            &shared,
            &Arc::new(JunkPatterns::default()),
            dir.path(),
            &journal,
            &AliasDictionary::empty(),
            "alpha",
            50,
        );
        // One mutation happened (the add), so the revision has advanced past zero.
        assert!(response.revision >= 1);
    }

    // Benchmark-style timing on a synthetic 5M-entry index shaped like the reference
    // machine's home: paths four components deep, mixed name lengths. Ignored by default (it
    // builds a large in-memory tree); run with `cargo test --release -- --ignored --nocapture`
    // to see per-query cost against the §10 Instant budget (≤50 ms for first results). Reports
    // sequential-vs-parallel full scan, typing-extension reuse, and a superseded-scan abort
    // (#26).
    #[test]
    #[ignore]
    fn five_million_entry_query_timing() {
        use std::time::Instant;

        let root = PathBuf::from("/home/bench");
        let shared = new_index(&root);
        let build_started = Instant::now();
        {
            let mut index = shared.write().unwrap();
            // 2500 top dirs × 5 modules × 5 src dirs × 80 files = 5,000,000 files, four
            // components deep, with file-name length varying by the counters.
            for a in 0..2500u32 {
                let top = index.add_dir(0, &format!("project{a:04}"), model::Tier::Normal, 0);
                for b in 0..5u32 {
                    let mid = index.add_dir(top, &format!("module{b}"), model::Tier::Normal, 0);
                    for c in 0..5u32 {
                        let leaf = index.add_dir(mid, &format!("src{c}"), model::Tier::Normal, 0);
                        for f in 0..80u32 {
                            index.add_file(
                                leaf,
                                &format!("file_{a:04}_{b}_{c}_{f:03}.rs"),
                                model::Tier::Normal,
                            );
                        }
                    }
                }
            }
            println!(
                "built {} entries in {:.2} s",
                index.len(),
                build_started.elapsed().as_secs_f64()
            );
        }

        let index = shared.read().unwrap();
        let ctx = RankContext::empty();
        let cores = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(1);
        println!(
            "scanning {} entries across {cores} shards\n",
            index.entries.len()
        );

        // Warm the allocator / caches with one selective query.
        let _ = query::run_impl(&index, &ctx, "file_2499_0_0_000", 50, None, None, cores);

        let bench = |label: &str, q: &str| {
            let seq_started = Instant::now();
            let seq = query::run_impl(&index, &ctx, q, 50, None, None, 1);
            let seq_ms = seq_started.elapsed().as_secs_f64() * 1000.0;
            let par_started = Instant::now();
            let par = query::run_impl(&index, &ctx, q, 50, None, None, cores);
            let par_ms = par_started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(seq.hits, par.hits, "sharding must not change results");
            println!(
                "{label:<26} q={q:<22?} {:>3} hits, scanned {:>8} | seq {seq_ms:7.2} ms  par {par_ms:7.2} ms",
                par.hits.len(),
                par.scanned
            );
        };

        // Short common query: matches nearly everything, hits the candidate cap and exits
        // early (no full scan).
        bench("short common (cap hit)", "file");
        // Long rare query: matches a handful, so it full-scans the whole index — the §10
        // worst case this ticket targets (≤25 ms parallel).
        bench("long rare (full scan)", "file_1999_4_4_079");
        // A query that matches nothing still full-scans; the true worst case.
        bench("miss (full scan)", "zzz_no_such_entry_zz");

        // Typing-extension reuse: extend a query four times. Without reuse each keystroke
        // full-scans; with reuse each rescans only the previous match set.
        let extensions = ["file_0007", "file_0007_", "file_0007_2", "file_0007_2_"];
        let full_started = Instant::now();
        for q in extensions {
            let _ = query::run_impl(&index, &ctx, q, 50, None, None, cores);
        }
        let full_ms = full_started.elapsed().as_secs_f64() * 1000.0;

        // Reuse chain: one full scan to seed, then each extension rescans only the previous
        // match set. Timed on its own; the equality checks against full scans run untimed
        // afterwards so they never pollute the measurement.
        let reuse_started = Instant::now();
        let base = query::run_impl(&index, &ctx, extensions[0], 50, None, None, cores);
        let mut slots = base.candidate_entries.clone();
        let mut collected = Vec::new();
        for q in &extensions[1..] {
            let out = query::run_impl(&index, &ctx, q, 50, Some(&slots), None, 1);
            slots = out.candidate_entries.clone();
            collected.push((*q, out));
        }
        let reuse_ms = reuse_started.elapsed().as_secs_f64() * 1000.0;

        assert!(
            base.exhaustive,
            "base query must be exhaustive to seed reuse"
        );
        for (q, out) in &collected {
            assert!(out.exhaustive);
            // A reuse result is byte-identical to a full scan for the same query.
            let full = query::run_impl(&index, &ctx, q, 50, None, None, cores);
            assert_eq!(out.hits, full.hits);
        }
        println!(
            "\ntyping sequence (4 queries):  full-scan each {full_ms:7.2} ms  |  reuse chain {reuse_ms:7.2} ms"
        );

        // Superseded-scan abort: a scan whose stamp is already behind the shared counter
        // aborts at the first checkpoint instead of walking all 5M entries.
        let generation = AtomicU64::new(9);
        let cancel = query::Cancel::new(&generation, 1);
        let abort_started = Instant::now();
        let aborted = query::run_impl(
            &index,
            &ctx,
            "zzz_no_such_entry_zz",
            50,
            None,
            Some(&cancel),
            cores,
        );
        let abort_ms = abort_started.elapsed().as_secs_f64() * 1000.0;
        assert!(aborted.aborted, "a superseded scan must abort");
        println!(
            "superseded abort:             scanned {:>8} entries then aborted in {abort_ms:7.2} ms",
            aborted.scanned
        );
    }
}
