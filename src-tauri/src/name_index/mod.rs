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
    sync::{Arc, OnceLock, RwLock},
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

/// Managed Tauri state holding the live index and everything needed to search, drain,
/// and persist it.
pub struct NameIndex {
    data: Arc<RwLock<IndexData>>,
    junk: Arc<JunkPatterns>,
    root: PathBuf,
    index_file: PathBuf,
    /// Local, ranking-only record of visits (SPEC §6, §11).
    journal: Arc<VisitJournal>,
    /// Directory holding the app's persisted data (aliases, journal, index).
    app_data_dir: PathBuf,
    /// The Alias Dictionary, loaded lazily on first search (hot-reload not required).
    aliases: OnceLock<Arc<AliasDictionary>>,
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

        let junk = Arc::new(JunkPatterns::default());
        let loaded = persist::load(&index_file, &root);
        let had_persisted = loaded.is_some();
        let data = Arc::new(RwLock::new(
            loaded.unwrap_or_else(|| IndexData::new(root.clone())),
        ));

        {
            let data = data.clone();
            let junk = junk.clone();
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
            app_data_dir,
            aliases: OnceLock::new(),
        })
    }

    /// The Alias Dictionary, loaded from `aliases.json` on first use (SPEC §6). Returns a
    /// cheap `Arc` clone so the search can move it into the blocking task.
    fn aliases(&self) -> Arc<AliasDictionary> {
        self.aliases
            .get_or_init(|| {
                let path = self.app_data_dir.join("aliases.json");
                Arc::new(AliasDictionary::load(&path, &self.root))
            })
            .clone()
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
    pub hits: Vec<SearchHit>,
}

/// Run a search, draining dirty Junk directories first when the query targets Junk.
fn run_search(
    data: &Arc<RwLock<IndexData>>,
    junk: &Arc<JunkPatterns>,
    root: &Path,
    journal: &VisitJournal,
    aliases: &AliasDictionary,
    query: &str,
    limit: usize,
) -> SearchResponse {
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
    SearchResponse {
        revision: index.revision,
        hits: query::search(&index, &ctx, query, limit),
    }
}

/// Query the Name Index. Runs off the async runtime's core threads (like `list_location`)
/// so the read scan and any Junk drain never block IPC.
#[tauri::command]
pub async fn search_name_index(
    query: String,
    limit: u32,
    state: State<'_, NameIndex>,
) -> Result<SearchResponse, String> {
    let data = state.data.clone();
    let junk = state.junk.clone();
    let root = state.root.clone();
    let journal = state.journal.clone();
    let aliases = state.aliases();
    tauri::async_runtime::spawn_blocking(move || {
        run_search(
            &data,
            &junk,
            &root,
            &journal,
            &aliases,
            &query,
            limit as usize,
        )
    })
    .await
    .map_err(|_| "name index search task failed".to_owned())
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
        let junk = Arc::new(JunkPatterns::default());
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

    // Benchmark-style timing on a synthetic 1M-entry index. Ignored by default (it builds
    // a large in-memory tree); run with `cargo test -- --ignored --nocapture` to see the
    // per-query cost against the §10 Instant budget (≤50 ms).
    #[test]
    #[ignore]
    fn million_entry_query_timing() {
        use std::time::Instant;

        let root = PathBuf::from("/home/bench");
        let shared = new_index(&root);
        {
            let mut index = shared.write().unwrap();
            // 1000 directories × 1000 files = 1,000,000 entries.
            for d in 0..1000u32 {
                let dir = index.add_dir(0, &format!("dir{d:04}"), model::Tier::Normal, 0);
                for f in 0..1000u32 {
                    index.add_file(dir, &format!("file{d:04}_{f:04}.txt"), model::Tier::Normal);
                }
            }
        }

        let index = shared.read().unwrap();
        let ctx = RankContext::empty();
        // Warm one selective query, then time several representative queries.
        let _ = query::search(&index, &ctx, "file0500_0500", 50);

        for query in ["file0500_0500", "file", "dir0999", "readme"] {
            let started = Instant::now();
            let hits = query::search(&index, &ctx, query, 50);
            let micros = started.elapsed().as_micros();
            println!(
                "query {query:?}: {} hits in {micros} µs ({:.2} ms)",
                hits.len(),
                micros as f64 / 1000.0
            );
        }
    }
}
