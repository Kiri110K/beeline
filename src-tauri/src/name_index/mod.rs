//! The Name Index (SPEC §6): an application-owned index of item names and paths — no
//! content — that answers Search Queries over the home directory. Mounted-volume
//! support is a later chunk; the index is keyed by a root path so that seam is clean.
//!
//! Concurrency: the index lives behind `Arc<RwLock<IndexData>>`. Searches take the read
//! lock and never block each other; the crawl, incremental apply, diff-rescan, and Junk
//! drain take short write locks (the initial crawl never holds it across IO — see
//! `crawl.rs`). Background work runs on plain `std::thread`s lowered to background QoS.

mod alias;
mod benchmark;
mod crawl;
mod junk;
mod junk_refresh;
mod model;
mod persist;
mod qgram;
mod query;
mod visit_journal;
mod watcher;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex, RwLock,
    },
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::json;
use tauri::{ipc::Channel, AppHandle, Emitter, Manager, State};

use crate::{recents::RecentsCache, telemetry::Telemetry};
use alias::AliasDictionary;
use junk::JunkPatterns;
use junk_refresh::JunkRefresh;
use model::{IndexData, Tier};
use qgram::QGramIndex;
use query::{RankContext, RetrievalSignals, RetrievalSource, SearchHit};
use visit_journal::{VisitJournal, VisitKind};

/// The volume key for the home root. Mounted volumes get their own keys later.
const HOME_VOLUME_KEY: &str = "home";
const QGRAM_READY_EVENT: &str = "beeline://search-qgram-ready";

/// The built-in Junk seed list (SPEC §6), re-exported so the settings store can seed a
/// fresh install's editable list from the same source (SPEC §12).
pub(crate) fn builtin_junk_names() -> Vec<String> {
    junk::default_names()
}

/// The Junk names introduced in seed version 2, applied to existing installs by the
/// settings migration (SPEC §12) so they gain the new builtins without losing user edits.
pub(crate) fn junk_seed_v2_names() -> Vec<String> {
    junk::V2_SEED_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

pub(crate) use benchmark::run as run_benchmark;

/// A swappable Junk-patterns handle. Search, the watcher, and the Junk drain each take a
/// cheap snapshot (`.read().clone()`); `apply_settings` swaps the inner `Arc` so a Settings
/// change reaches every future classification without re-crawling (Junk refreshes lazily,
/// SPEC §6, §10).
type SharedJunk = Arc<RwLock<Arc<JunkPatterns>>>;
type SharedQGrams = Arc<RwLock<Option<Arc<QGramIndex>>>>;

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
    /// Monotonic query counter (SPEC §10 cancel-on-newer). Search v2 bumps it on
    /// entry; each running scan carries its own stamp and aborts once a newer query moves the
    /// counter past it.
    generation: Arc<AtomicU64>,
    /// The single previous exhaustive scan (the app has one search stream, SPEC §5), so a
    /// strictly-extending keystroke rescans just that match set instead of the whole index.
    reuse: Arc<Mutex<Option<ReuseCache>>>,
    /// Search v2 Typo Correction candidates for the immutable mapped base. Built in the
    /// background and swapped atomically; overlay Items are verified separately.
    qgrams: SharedQGrams,
    /// Event-driven battery policy for lazy Junk refreshes (SPEC §10).
    junk_refresh: JunkRefresh,
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
        let qgram_file = index_file.with_extension("qgram");

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
        let qgrams: SharedQGrams = Arc::new(RwLock::new(None));
        if had_persisted {
            load_or_build_qgrams(
                data.clone(),
                index_file.clone(),
                root.clone(),
                qgram_file.clone(),
                qgrams.clone(),
                app.clone(),
            );
        }
        if had_persisted {
            // The validation stamp keeps cold launch bounded by avoiding a 310+ MiB
            // checksum walk. Warm the ranker's worker paths and a representative slice of
            // Item/name pages now, while startup still has ample budget, so that the first
            // real keystroke does not inherit that cold-page cost. `g` also exercises the
            // RU-layout variant and consistently reaches the candidate cap on the reference
            // index without making the entire mapping resident.
            let started = Instant::now();
            let index = data.read().expect("name index lock poisoned");
            let outcome = query::run(&index, &RankContext::empty(), "g", 50, None, None);
            record(
                app,
                "search_prewarmed",
                json!({
                    "duration_ms": u64::try_from(started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    "scanned": outcome.scanned,
                }),
            );
        }
        let junk_refresh =
            JunkRefresh::new(data.clone(), root.clone(), junk.clone(), Some(app.clone()));
        let (watcher_start_tx, watcher_start_rx) = mpsc::channel();
        let watcher_ready = watcher::spawn_deferred(
            data.clone(),
            root.clone(),
            junk.clone(),
            Some(junk_refresh.clone()),
            Some(app.clone()),
            watcher_start_rx,
        );

        {
            let data = data.clone();
            let crawl_junk = junk.read().expect("junk lock poisoned").clone();
            let root = root.clone();
            let app = app.clone();
            let index_file = index_file.clone();
            let qgram_file = qgram_file.clone();
            let qgrams = qgrams.clone();
            thread::spawn(move || {
                crawl::set_crawl_qos();
                // The watcher is registered before the crawl begins, so changes made during
                // the crawl are queued. Applying them waits until compaction finishes below.
                let _ = watcher_ready.recv();
                if had_persisted {
                    let entries = data.read().expect("name index lock poisoned").len();
                    record(&app, "index_loaded", json!({ "entries": entries }));
                    crawl::diff_rescan(&data, root.clone(), &crawl_junk, Some(&app));
                    let index = data.read().expect("name index lock poisoned");
                    // Keep the startup delta in the mutable overlay for this session. A
                    // full 5M-entry rewrite makes the old mmap and the growing temp file
                    // resident together; repeating the now-subsecond diff next launch is
                    // safer until v4 gains bounded incremental compaction.
                    record(
                        &app,
                        "index_persist_deferred",
                        json!({ "entries": index.len(), "mutations": index.revision }),
                    );
                } else {
                    crawl::initial_crawl(&data, root.clone(), &crawl_junk, Some(&app));
                    let persist_started = Instant::now();
                    match persist::save(&data, &index_file) {
                        Ok(()) => record(
                            &app,
                            "index_persist_finished",
                            json!({
                                "duration_ms": u64::try_from(
                                    persist_started.elapsed().as_millis()
                                )
                                .unwrap_or(u64::MAX),
                            }),
                        ),
                        Err(error) => {
                            record(
                                &app,
                                "index_persist_failed",
                                json!({ "error": error.to_string() }),
                            );
                            eprintln!("name index persist after crawl failed: {error}");
                        }
                    }
                    load_or_build_qgrams(
                        data.clone(),
                        index_file,
                        root,
                        qgram_file,
                        qgrams,
                        app.clone(),
                    );
                }
                // Applying FSEvents concurrently with the first crawl can discover the same
                // large subtree twice, hold the write lock for minutes, and starve the first
                // Search Query. Release the queued events only after the mapped base is live.
                let _ = watcher_start_tx.send(());
            });
        }
        {
            let junk_refresh = junk_refresh.clone();
            crate::power::spawn_monitor(move |source| {
                junk_refresh.power_source_changed(source);
            });
        }

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
            qgrams,
            junk_refresh,
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

fn load_or_build_qgrams(
    data: Arc<RwLock<IndexData>>,
    source_path: PathBuf,
    root: PathBuf,
    path: PathBuf,
    target: SharedQGrams,
    app: AppHandle,
) {
    let existing = {
        let index = data.read().expect("name index lock poisoned");
        index
            .base_content_hash()
            .and_then(|hash| QGramIndex::open(&path, hash, index.base_entry_count()).map(Arc::new))
    };
    if let Some(existing) = existing {
        *target.write().expect("q-gram lock poisoned") = Some(existing);
        record(&app, "search_qgram_loaded", json!({ "rebuilt": false }));
        return;
    }

    thread::spawn(move || {
        // The two build passes allocate roughly the final sidecar size. macOS's allocator
        // retains those freed pages in a long-lived GUI process, so perform the one-off build
        // in the same executable's CLI-only helper mode. Its entire heap disappears on exit;
        // the app only maps the finished read-only sidecar.
        let started = Instant::now();
        let built = std::env::current_exe()
            .map_err(|error| format!("cannot resolve q-gram helper executable: {error}"))
            .and_then(|executable| {
                Command::new(executable)
                    .arg("--build-search-qgram")
                    .arg(&source_path)
                    .arg(&root)
                    .arg(&path)
                    .status()
                    .map_err(|error| format!("cannot launch q-gram helper: {error}"))
            })
            .and_then(|status| {
                status
                    .success()
                    .then_some(())
                    .ok_or_else(|| format!("q-gram helper exited with {status}"))
            })
            .and_then(|()| {
                let index = data.read().expect("name index lock poisoned");
                let hash = index
                    .base_content_hash()
                    .ok_or_else(|| "Name Index base disappeared".to_owned())?;
                let entries = index.base_entry_count();
                let mapped = QGramIndex::open(&path, hash, entries)
                    .ok_or_else(|| "built q-gram sidecar is invalid or stale".to_owned())?;
                Ok((Arc::new(mapped), hash, entries))
            });
        match built {
            Ok((mapped, hash, entries)) => {
                let still_current = {
                    let index = data.read().expect("name index lock poisoned");
                    index.base_content_hash() == Some(hash) && index.base_entry_count() == entries
                };
                if !still_current {
                    record(&app, "search_qgram_stale_build", json!({}));
                    return;
                }
                let bytes = std::fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                let postings = mapped.postings();
                *target.write().expect("q-gram lock poisoned") = Some(mapped);
                record(
                    &app,
                    "search_qgram_built",
                    json!({
                        "duration_ms": u64::try_from(started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        "bytes": bytes,
                        "postings": postings,
                    }),
                );
                if let Err(error) = app.emit(QGRAM_READY_EVENT, json!({})) {
                    record(
                        &app,
                        "search_qgram_ready_emit_failed",
                        json!({ "error": error.to_string() }),
                    );
                }
            }
            Err(error) => {
                record(
                    &app,
                    "search_qgram_failed",
                    json!({ "error": error.to_string() }),
                );
                eprintln!("q-gram sidecar build failed: {error}");
            }
        }
    });
}

/// CLI-only q-gram builder used by the app's short-lived helper process. It maps the
/// persisted Name Index independently and never initializes Tauri or a WebView.
pub fn build_qgram_sidecar(source_path: &Path, root: &Path, path: &Path) -> Result<(), String> {
    crate::qos::set_qos(0x11);
    let source = persist::load(source_path, root)
        .ok_or_else(|| "cannot map q-gram source Name Index".to_owned())?;
    QGramIndex::build(&source, path)
        .map(|_| ())
        .map_err(|error| format!("cannot build q-gram sidecar: {error}"))
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
    /// Time spent inside the blocking backend task. Comparing it with frontend telemetry
    /// separates index-scan cost from task-queue, IPC, and WebView wake-up latency.
    pub backend_duration_ms: u64,
    pub hits: Vec<SearchHit>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SearchWave {
    pub stage: &'static str,
    pub revision: u64,
    pub complete: bool,
    pub qgram_ready: bool,
    pub scanned: usize,
    pub backend_duration_ms: u64,
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
/// set. It must strictly extend `prev_lower` — which keeps the match set a subset — *and*
/// preserve the scan's matching semantics, so the cache stays a superset of the new matches.
///
/// Two ways an extension can break the superset property, each a fall-back to a full scan.
/// First, a new `/` re-shapes a plain query into a path-shaped one (or adds a path segment),
/// changing the scan's scope entirely. (A leading `~` cannot appear via extension, since the
/// prefix is shared, so path-shapedness is otherwise preserved.) Second, crossing the
/// single-token → multi-token boundary: a single-token query matches names only (unchanged
/// pre-tokenization behavior), whereas every token of a multi-token query may ALSO match the
/// path (SPEC §6 token contract), so the single-token cache holds only name-matched entries
/// and is NOT a superset of the multi-token set. Within multi-token, appending either extends
/// the last token or, after a space, adds a token; both only narrow the AND, so the cache
/// stays a superset. A trailing space is already trimmed off `next_lower`, so it never reaches
/// here as a spurious new token.
///
/// Layout correction survives either preserved case: the corrected variant of the extended
/// query extends the corrected variant of the prefix (the key map is per-character), so every
/// corrected match of `next_lower` is already in `prev_lower`'s cached set.
fn can_extend(prev_lower: &str, next_lower: &str) -> bool {
    if prev_lower.is_empty()
        || next_lower.len() <= prev_lower.len()
        || !next_lower.starts_with(prev_lower)
        || next_lower[prev_lower.len()..].contains('/')
    {
        return false;
    }
    // The single→multi boundary only matters for plain queries; a path-shaped `next` (which,
    // given the shared prefix and the no-new-`/` check above, means `prev` was already
    // path-shaped) keeps spaces literal and is unaffected by tokenization.
    let next_path_shaped = next_lower.contains('/') || next_lower.starts_with('~');
    if !next_path_shaped {
        let prev_multi = prev_lower.contains(char::is_whitespace);
        let next_multi = next_lower.contains(char::is_whitespace);
        if !prev_multi && next_multi {
            return false;
        }
    }
    true
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
    junk_refresh: Option<&JunkRefresh>,
    query: &str,
    limit: usize,
    reuse: Option<&ReuseCache>,
    cancel: Option<&query::Cancel>,
    retrieval: RetrievalSignals,
) -> Executed {
    let query_lower = query.trim().to_lowercase();
    if junk.query_targets_junk(&query_lower) {
        if let Some(junk_refresh) = junk_refresh {
            junk_refresh.targeting_query();
        } else {
            let _ = crawl::drain_junk_dirty(data, root, junk);
        }
    }
    let aggregate = journal.aggregate();
    let ctx = RankContext {
        journal: &aggregate,
        aliases,
        retrieval,
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

/// Resolve direct navigation before it can queue behind background preview work in the
/// shared blocking pool. Exact existing paths are already the ranker's strongest possible
/// answer, so they need neither the Name Index read lock nor a full scan.
fn direct_existing_path_response(
    data: &Arc<RwLock<IndexData>>,
    junk: &JunkPatterns,
    root: &Path,
    query: &str,
    limit: usize,
    started: Instant,
) -> Option<SearchResponse> {
    if limit == 0 {
        return None;
    }
    let (target, is_directory) = query::existing_typed_path(query.trim(), root)?;
    let tier = target
        .strip_prefix(root)
        .ok()
        .map(|relative| junk.classify(relative))
        .unwrap_or(Tier::Normal);
    let path = target.to_string_lossy().into_owned();
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let revision = data.try_read().map(|index| index.revision).unwrap_or(0);
    Some(SearchResponse {
        revision,
        reused: false,
        scanned: 0,
        backend_duration_ms: started.elapsed().as_millis() as u64,
        hits: vec![SearchHit {
            name,
            path,
            is_directory,
            tier: tier.as_str(),
        }],
    })
}

/// Run a search with no cancellation or reuse (the direct path the tests exercise).
/// Production searches go through Search v2, which adds stale-cancellation and
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
    let started = Instant::now();
    let executed = execute_search(
        data,
        junk,
        root,
        journal,
        aliases,
        None,
        query,
        limit,
        None,
        None,
        RetrievalSignals::default(),
    );
    SearchResponse {
        revision: executed.revision,
        reused: executed.reused,
        scanned: executed.outcome.scanned,
        backend_duration_ms: started.elapsed().as_millis() as u64,
        hits: executed.outcome.hits,
    }
}

struct WorkingSet {
    slots: Vec<u32>,
    retrieval: RetrievalSignals,
}

fn working_set(
    index: &IndexData,
    current_location: Option<&Path>,
    pinned_paths: &[PathBuf],
    recent_paths: &[PathBuf],
    journal: &visit_journal::Aggregate,
) -> WorkingSet {
    let mut slots = BTreeSet::new();
    let mut retrieval = RetrievalSignals::default();
    if let Some(location) = current_location {
        if let Some(directory) = index.resolve_dir(location) {
            for slot in index.direct_entry_slots(directory) {
                slots.insert(slot);
                retrieval.insert(slot, RetrievalSource::CurrentLocation);
            }
        }
    }
    for path in pinned_paths {
        if let Some(slot) = index.resolve_item_slot(path) {
            slots.insert(slot);
            retrieval.insert(slot, RetrievalSource::PinnedAnchor);
        }
    }
    for path in recent_paths {
        if let Some(slot) = index.resolve_item_slot(path) {
            slots.insert(slot);
            retrieval.insert(slot, RetrievalSource::Recents);
        }
    }
    for path in journal.paths().map(Path::new) {
        if let Some(slot) = index.resolve_item_slot(path) {
            slots.insert(slot);
        }
    }
    WorkingSet {
        slots: slots.into_iter().collect(),
        retrieval,
    }
}

fn send_wave(channel: &Channel<SearchWave>, wave: SearchWave) -> Result<(), String> {
    channel
        .send(wave)
        .map_err(|error| format!("failed to send Search v2 wave: {error}"))
}

/// Search v2 sends one ranked stream through a Tauri channel. The frontend may paint the
/// complete Working Set before global work finishes, then reconcile exact/layout and fuzzy
/// waves by stable Item path. One generation stamp covers the entire stream.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn search_name_index_v2(
    query: String,
    limit: u32,
    current_location: Option<String>,
    pinned_paths: Vec<String>,
    on_wave: Channel<SearchWave>,
    app: AppHandle,
    state: State<'_, NameIndex>,
    recents: State<'_, RecentsCache>,
) -> Result<(), String> {
    let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let started = Instant::now();
    let data = state.data.clone();
    let junk = state.junk_snapshot();
    let root = state.root.clone();
    if let Some(response) =
        direct_existing_path_response(&data, &junk, &root, &query, limit as usize, started)
    {
        *state.reuse.lock().expect("reuse cache lock poisoned") = None;
        return send_wave(
            &on_wave,
            SearchWave {
                stage: "exact",
                revision: response.revision,
                complete: true,
                qgram_ready: state.qgrams.read().expect("q-gram lock poisoned").is_some(),
                scanned: response.scanned,
                backend_duration_ms: response.backend_duration_ms,
                hits: response.hits,
            },
        );
    }

    let journal = state.journal.clone();
    let aliases = state.aliases_snapshot();
    let generation = state.generation.clone();
    let reuse = state.reuse.clone();
    let junk_refresh = state.junk_refresh.clone();
    let qgrams = state.qgrams.clone();
    let recent_paths = recents.paths();
    let current_location = current_location.map(PathBuf::from);
    let pinned_paths = pinned_paths
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    tauri::async_runtime::spawn_blocking(move || {
        crate::qos::set_user_initiated_qos();
        let stream_started = Instant::now();
        let cancel = query::Cancel::new(&generation, my_gen);
        let aggregate = journal.aggregate();
        let (working, working_slots, context, revision) = {
            let index = data.read().expect("name index lock poisoned");
            let WorkingSet { slots, retrieval } = working_set(
                &index,
                current_location.as_deref(),
                &pinned_paths,
                &recent_paths,
                &aggregate,
            );
            let context = RankContext {
                journal: &aggregate,
                aliases: &aliases,
                retrieval,
            };
            let outcome = query::run_fuzzy(
                &index,
                &context,
                &query,
                limit as usize,
                &slots,
                Some(&cancel),
                true,
            );
            (outcome, slots, context, index.revision)
        };
        if working.aborted {
            return Err("search superseded".to_owned());
        }
        let working_empty = working.hits.is_empty();
        let global = qgram::significant_len(&query) >= 5;
        let qgram_ready = qgrams.read().expect("q-gram lock poisoned").is_some();
        send_wave(
            &on_wave,
            SearchWave {
                stage: "working_set",
                revision,
                complete: !global,
                qgram_ready,
                scanned: working.scanned,
                backend_duration_ms: u64::try_from(stream_started.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
                hits: working.hits,
            },
        )?;
        if !global {
            return Ok(());
        }

        let qgram = qgrams.read().expect("q-gram lock poisoned").clone();
        if let Some(qgram) = qgram.filter(|_| qgram::supports_query(&query)) {
            if junk.query_targets_junk(&query.trim().to_lowercase()) {
                junk_refresh.targeting_query();
            }
            // When the complete Working Set has no match, verify the final token's q-gram
            // pool first. This is the ordered Path Interpretation's likely name pool (for
            // example `methodology` in `vault methodology`), and avoids making the first
            // useful row wait for the much larger ordinary-search union.
            let mut priority_candidates = 0usize;
            let mut priority_results = 0usize;
            let mut priority_posting_visits = 0usize;
            let mut priority_sent = false;
            if working_empty {
                if let Some(seed) = qgram.ordered_tail_literal_candidates(&query, &cancel) {
                    if seed.aborted {
                        return Err("search superseded".to_owned());
                    }
                    priority_posting_visits = seed.posting_visits;
                    let mut priority_slots = seed.slots;
                    {
                        let index = data.read().expect("name index lock poisoned");
                        let overlay = query::ordered_tail_literal_slots(
                            &index,
                            &query,
                            qgram.source_entries(),
                            Some(&cancel),
                        );
                        if overlay.aborted {
                            return Err("search superseded".to_owned());
                        }
                        priority_slots.extend(overlay.slots);
                    }
                    priority_slots.sort_unstable();
                    priority_slots.dedup();
                    priority_candidates = priority_slots.len();
                    let priority = {
                        let index = data.read().expect("name index lock poisoned");
                        query::run_fuzzy(
                            &index,
                            &context,
                            &query,
                            limit as usize,
                            &priority_slots,
                            Some(&cancel),
                            false,
                        )
                    };
                    if priority.aborted {
                        return Err("search superseded".to_owned());
                    }
                    priority_results = priority.hits.len();
                    if !priority.hits.is_empty() {
                        send_wave(
                            &on_wave,
                            SearchWave {
                                stage: "fuzzy",
                                revision,
                                complete: false,
                                qgram_ready: true,
                                scanned: priority.scanned,
                                backend_duration_ms: u64::try_from(
                                    stream_started.elapsed().as_millis(),
                                )
                                .unwrap_or(u64::MAX),
                                hits: priority.hits,
                            },
                        )?;
                        priority_sent = true;
                    }
                }
            }
            let candidate_set = qgram.candidates(&query, &cancel);
            if candidate_set.aborted {
                return Err("search superseded".to_owned());
            }
            let mut candidate_slots = candidate_set.slots;
            {
                let index = data.read().expect("name index lock poisoned");
                candidate_slots
                    .extend((qgram.source_entries()..index.slot_len()).map(|slot| slot as u32));
            }
            candidate_slots.extend(working_slots);
            candidate_slots.sort_unstable();
            candidate_slots.dedup();

            let mut partial_error = None;
            let mut send_partial = |hits: Vec<SearchHit>, scanned: usize| {
                // Keep the fast ordered-tail rows stable until the deterministic final wave.
                // A partial from an arbitrary global shard can be less complete and would
                // otherwise replace the useful early result with an unrelated subset.
                if priority_sent || hits.is_empty() || partial_error.is_some() {
                    return;
                }
                if let Err(error) = send_wave(
                    &on_wave,
                    SearchWave {
                        stage: "fuzzy",
                        revision,
                        complete: false,
                        qgram_ready: true,
                        scanned,
                        backend_duration_ms: u64::try_from(stream_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        hits,
                    },
                ) {
                    partial_error = Some(error);
                }
            };
            let fuzzy = {
                let index = data.read().expect("name index lock poisoned");
                query::run_fuzzy_streaming(
                    &index,
                    &context,
                    &query,
                    limit as usize,
                    &candidate_slots,
                    Some(&cancel),
                    false,
                    &mut send_partial,
                )
            };
            if let Some(error) = partial_error {
                return Err(error);
            }
            if fuzzy.aborted {
                return Err("search superseded".to_owned());
            }
            let scanned = fuzzy.scanned;
            let hits = fuzzy.hits;
            let result_count = hits.len();
            send_wave(
                &on_wave,
                SearchWave {
                    stage: "fuzzy",
                    revision,
                    complete: true,
                    qgram_ready: true,
                    scanned,
                    backend_duration_ms: u64::try_from(stream_started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    hits,
                },
            )?;
            record(
                &app,
                "search_v2_completed",
                json!({
                    "duration_ms": u64::try_from(stream_started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    "candidates": candidate_slots.len(),
                    "posting_visits": candidate_set.posting_visits,
                    "priority_candidates": priority_candidates,
                    "priority_posting_visits": priority_posting_visits,
                    "priority_results": priority_results,
                    "results": result_count,
                    "retrieval": "qgram",
                }),
            );
            return Ok(());
        }

        // Until the sidecar is ready, and for token mixtures too short to have complete
        // q-gram recall, retain the exact/layout scanner as a correctness fallback.
        let cached = reuse.lock().expect("reuse cache lock poisoned").clone();
        let exact = execute_search(
            &data,
            &junk,
            &root,
            &journal,
            &aliases,
            Some(&junk_refresh),
            &query,
            limit as usize,
            cached.as_ref(),
            Some(&cancel),
            context.retrieval.clone(),
        );
        if exact.outcome.aborted {
            return Err("search superseded".to_owned());
        }
        {
            let mut slot = reuse.lock().expect("reuse cache lock poisoned");
            if exact.outcome.exhaustive {
                *slot = Some(ReuseCache {
                    query_lower: query.trim().to_lowercase(),
                    revision: exact.revision,
                    entries: exact.outcome.candidate_entries.clone(),
                });
            } else {
                *slot = None;
            }
        }

        let exact_hits = exact.outcome.hits;
        let result_count = exact_hits.len();
        send_wave(
            &on_wave,
            SearchWave {
                stage: "exact",
                revision: exact.revision,
                complete: true,
                qgram_ready,
                scanned: exact.outcome.scanned,
                backend_duration_ms: u64::try_from(stream_started.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
                hits: exact_hits,
            },
        )?;
        record(
            &app,
            "search_v2_completed",
            json!({
                "duration_ms": u64::try_from(stream_started.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
                "candidates": exact.outcome.scanned,
                "posting_visits": 0,
                "results": result_count,
                "exact_reused": exact.reused,
                "retrieval": "exact_fallback",
            }),
        );
        Ok(())
    })
    .await
    .map_err(|_| "Search v2 task failed".to_owned())?
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
    fn incremental_rename_recreate_and_type_changes_replace_old_entries() {
        let dir = TempDir::new();
        let path = dir.path().join("changing");
        touch(&path);
        let junk = JunkPatterns::default();
        let shared = new_index(dir.path());
        crawl::initial_crawl(&shared, dir.path().to_path_buf(), &junk, None);

        fs::remove_file(&path).unwrap();
        touch(&path.join("inside.txt"));
        let to_directory = {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &path, &junk)
        };
        assert_eq!(to_directory.added, 2);
        assert_eq!(to_directory.removed, 1);
        assert!(to_directory.indexed_subtree);
        assert!(shared.read().unwrap().resolve_dir(&path).is_some());
        assert_eq!(tier_of(&shared, "inside.txt"), Some("normal"));

        fs::remove_dir_all(&path).unwrap();
        touch(&path);
        let to_file = {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &path, &junk)
        };
        assert_eq!(to_file.added, 1);
        assert_eq!(to_file.removed, 2);
        assert!(!to_file.indexed_subtree);
        let index = shared.read().unwrap();
        assert!(index.resolve_dir(&path).is_none());
        assert!(index.has_child(0, "changing"));
        drop(index);

        let renamed = dir.path().join("renamed");
        fs::rename(&path, &renamed).unwrap();
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &renamed, &junk);
            crawl::apply_fs_event(&mut index, &path, &junk);
        }
        let index = shared.read().unwrap();
        assert!(!index.has_child(0, "changing"));
        assert!(index.has_child(0, "renamed"));
        drop(index);

        fs::remove_file(&renamed).unwrap();
        {
            let mut index = shared.write().unwrap();
            assert_eq!(
                crawl::apply_fs_event(&mut index, &renamed, &junk).removed,
                1
            );
        }
        touch(&renamed);
        {
            let mut index = shared.write().unwrap();
            assert_eq!(crawl::apply_fs_event(&mut index, &renamed, &junk).added, 1);
        }
        assert!(shared.read().unwrap().has_child(0, "renamed"));
    }

    #[test]
    fn dir_event_on_existing_dir_does_not_duplicate() {
        // Regression: a directory FSEvent must reconcile direct children, not re-index the
        // subtree — two events on an unchanged dir leave the entry count untouched.
        let dir = TempDir::new();
        touch(&dir.path().join("sub/a.txt"));
        touch(&dir.path().join("sub/b.txt"));
        let junk = JunkPatterns::default();
        let shared = new_index(dir.path());
        crawl::initial_crawl(&shared, dir.path().to_path_buf(), &junk, None);
        let baseline = shared.read().unwrap().len();

        let subdir = dir.path().join("sub");
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &subdir, &junk);
            crawl::apply_fs_event(&mut index, &subdir, &junk);
        }
        assert_eq!(shared.read().unwrap().len(), baseline);
    }

    #[test]
    fn dir_event_indexes_only_the_added_file() {
        // A file that appears on disk between two events yields exactly one new entry.
        let dir = TempDir::new();
        touch(&dir.path().join("sub/a.txt"));
        let junk = JunkPatterns::default();
        let shared = new_index(dir.path());
        crawl::initial_crawl(&shared, dir.path().to_path_buf(), &junk, None);
        let baseline = shared.read().unwrap().len();

        let subdir = dir.path().join("sub");
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &subdir, &junk);
        }
        touch(&subdir.join("c.txt"));
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &subdir, &junk);
        }
        assert_eq!(shared.read().unwrap().len(), baseline + 1);
        assert_eq!(tier_of(&shared, "c.txt"), Some("normal"));
    }

    #[test]
    fn wide_new_subtree_is_indexed_once() {
        let dir = TempDir::new();
        touch(&dir.path().join("existing.txt"));
        let junk = JunkPatterns::default();
        let shared = new_index(dir.path());
        crawl::initial_crawl(&shared, dir.path().to_path_buf(), &junk, None);

        let index_dir = TempDir::new();
        let file = index_dir.path().join("wide.idx");
        persist::save(&shared, &file).unwrap();
        let mapped = persist::load(&file, dir.path()).unwrap();
        let shared = Arc::new(RwLock::new(mapped));

        for number in 0..2_000 {
            fs::create_dir_all(dir.path().join(format!("incoming/dir-{number:04}"))).unwrap();
        }
        crawl::diff_rescan(&shared, dir.path().to_path_buf(), &junk, None);

        let index = shared.read().unwrap();
        let incoming = index.resolve_dir(&dir.path().join("incoming")).unwrap();
        assert_eq!(index.child_dirs(incoming).len(), 2_000);
        assert_eq!(index.len(), 2_002);
    }

    #[test]
    fn dir_event_removes_the_deleted_file() {
        // A file deleted on disk loses its entry on the next event over its parent dir.
        let dir = TempDir::new();
        touch(&dir.path().join("sub/a.txt"));
        touch(&dir.path().join("sub/b.txt"));
        let junk = JunkPatterns::default();
        let shared = new_index(dir.path());
        crawl::initial_crawl(&shared, dir.path().to_path_buf(), &junk, None);
        let baseline = shared.read().unwrap().len();

        let subdir = dir.path().join("sub");
        fs::remove_file(subdir.join("a.txt")).unwrap();
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &subdir, &junk);
        }
        assert_eq!(shared.read().unwrap().len(), baseline - 1);
        assert_eq!(tier_of(&shared, "a.txt"), None);
    }

    #[test]
    fn removing_a_deep_directory_tree_does_not_use_the_call_stack() {
        let dir = TempDir::new();
        let mut index = IndexData::new(dir.path().to_path_buf());
        let first = index.add_dir_known_absent(0, "level-0", model::Tier::Normal, 0);
        let mut parent = first;
        for level in 1..20_000 {
            parent = index.add_dir_known_absent(
                parent,
                &format!("level-{level}"),
                model::Tier::Normal,
                0,
            );
        }

        assert_eq!(index.len(), 20_000);
        assert_eq!(index.remove_child_count(0, "level-0"), 20_000);
        assert_eq!(index.len(), 0);
        assert!(index.child_dirs(0).is_empty());
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
        assert_eq!(loaded.slot_len(), before);
        assert!((0..loaded.slot_len()).all(|slot| loaded.entry(slot).is_some()));

        let reloaded = Arc::new(RwLock::new(loaded));
        assert_eq!(tier_of(&reloaded, "index.js"), Some("junk"));
        assert_eq!(tier_of(&reloaded, "secret.txt"), Some("hidden"));

        // A root mismatch is rejected (per-volume identity).
        assert!(persist::load(&file, Path::new("/nonexistent/root")).is_none());
    }

    #[test]
    fn persistence_rejects_v3_and_corrupt_v4() {
        let dir = TempDir::new();
        let old = dir.path().join("old.idx");
        let mut v3_header = Vec::from(*b"BLNI");
        v3_header.extend_from_slice(&3u32.to_le_bytes());
        fs::write(&old, v3_header).unwrap();
        assert!(persist::load(&old, dir.path()).is_none());

        let shared = new_index(dir.path());
        shared
            .write()
            .unwrap()
            .add_file(0, "kept.txt", model::Tier::Normal);
        let corrupt = dir.path().join("corrupt.idx");
        persist::save(&shared, &corrupt).unwrap();
        let mut bytes = fs::read(&corrupt).unwrap();
        let last = bytes.last_mut().unwrap();
        *last ^= 0x80;
        fs::write(&corrupt, bytes).unwrap();
        assert!(persist::load(&corrupt, dir.path()).is_none());
    }

    #[test]
    fn mapped_base_overlay_rebuild_round_trip() {
        let dir = TempDir::new();
        let file = dir.path().join("overlay.idx");
        let shared = new_index(dir.path());
        {
            let mut index = shared.write().unwrap();
            let docs = index.add_dir(0, "docs", model::Tier::Normal, 10);
            index.add_file(docs, "old.txt", model::Tier::Normal);
        }
        persist::save(&shared, &file).unwrap();
        {
            let mut index = shared.write().unwrap();
            let docs = index.resolve_dir(&dir.path().join("docs")).unwrap();
            assert!(index.remove_child(docs, "old.txt"));
            index.add_file(docs, "new.txt", model::Tier::Normal);
            index.set_node_mtime(docs, 99);
        }
        persist::save(&shared, &file).unwrap();
        let loaded = persist::load(&file, dir.path()).unwrap();
        let docs = loaded.resolve_dir(&dir.path().join("docs")).unwrap();
        assert!(!loaded.has_child(docs, "old.txt"));
        assert!(loaded.has_child(docs, "new.txt"));
        assert_eq!(loaded.node(docs).unwrap().mtime_ms, 99);
    }

    #[test]
    fn removing_a_mapped_directory_unlinks_its_node_tree() {
        let dir = TempDir::new();
        let file = dir.path().join("mapped-removal.idx");
        let shared = new_index(dir.path());
        let docs = {
            let mut index = shared.write().unwrap();
            let docs = index.add_dir(0, "docs", model::Tier::Normal, 10);
            let nested = index.add_dir(docs, "nested", model::Tier::Normal, 20);
            index.add_file(nested, "old.txt", model::Tier::Normal);
            docs
        };
        persist::save(&shared, &file).unwrap();

        let mut loaded = persist::load(&file, dir.path()).unwrap();
        assert_eq!(loaded.remove_child_count(0, "docs"), 3);
        assert_eq!(loaded.len(), 0);
        assert!(loaded.node(docs).is_none());
        assert!(loaded.child_dirs(0).is_empty());

        let shared = Arc::new(RwLock::new(loaded));
        persist::save(&shared, &file).unwrap();
        let reloaded = persist::load(&file, dir.path()).unwrap();
        assert_eq!(reloaded.len(), 0);
        assert!(reloaded.child_dirs(0).is_empty());
    }

    #[test]
    fn mapped_base_does_not_expose_records_as_overlay_children() {
        let dir = TempDir::new();
        let file = dir.path().join("mapped-overlay.idx");
        let shared = new_index(dir.path());
        {
            let mut index = shared.write().unwrap();
            let base = index.add_dir(0, "base", model::Tier::Normal, 10);
            index.add_file(base, "anchor.txt", model::Tier::Normal);
            index.add_file(0, "root.txt", model::Tier::Normal);
        }
        persist::save(&shared, &file).unwrap();

        let mut loaded = persist::load(&file, dir.path()).unwrap();
        let overlay = loaded.add_dir_known_absent(0, "overlay", model::Tier::Normal, 20);
        loaded.add_file(overlay, "only-child.txt", model::Tier::Normal);

        assert_eq!(
            loaded.direct_children(overlay),
            vec![("only-child.txt".to_owned(), false)]
        );
        assert_eq!(loaded.remove_child_count(overlay, "only-child.txt"), 1);
        assert!(loaded.direct_children(overlay).is_empty());
        assert!(loaded.has_child(0, "base"));
        assert!(loaded.has_child(0, "root.txt"));
        assert!(loaded.has_child(0, "overlay"));
    }

    #[test]
    fn mapped_incremental_parent_then_child_events_do_not_remove_other_entries() {
        let dir = TempDir::new();
        let file = dir.path().join("mapped-event-order.idx");
        let shared = new_index(dir.path());
        {
            let mut index = shared.write().unwrap();
            let base = index.add_dir(0, "base", model::Tier::Normal, 10);
            index.add_file(base, "anchor.txt", model::Tier::Normal);
        }
        persist::save(&shared, &file).unwrap();

        let fresh = dir.path().join("fresh");
        touch(&fresh.join("dir-000/file-00.txt"));
        let junk = JunkPatterns::default();
        let mut loaded = persist::load(&file, dir.path()).unwrap();
        let before = loaded.len();

        let parent_stats = crawl::apply_fs_event(&mut loaded, &fresh, &junk);
        assert_eq!(parent_stats.added, 3);
        assert_eq!(parent_stats.removed, 0);
        assert!(parent_stats.indexed_subtree);
        let after_parent = loaded.len();
        assert_eq!(after_parent, before + 3);

        let child_stats = crawl::apply_fs_event(&mut loaded, &fresh.join("dir-000"), &junk);
        assert_eq!(child_stats, crawl::FsApplyStats::default());
        assert_eq!(loaded.len(), after_parent);
        assert!(loaded.has_child(0, "base"));
        assert!(loaded.resolve_dir(&fresh.join("dir-000")).is_some());
        assert!(loaded
            .resolve_dir(&fresh.join("dir-000"))
            .is_some_and(|dir_id| loaded.has_child(dir_id, "file-00.txt")));
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

    #[test]
    fn junk_refresh_defers_on_battery_and_drains_on_external_power() {
        let dir = TempDir::new();
        let root = dir.path();
        touch(&root.join("node_modules/pkg/index.js"));
        let patterns = JunkPatterns::default();
        let shared = new_index(root);
        crawl::initial_crawl(&shared, root.to_path_buf(), &patterns, None);

        let new_file = root.join("node_modules/pkg/from-battery.js");
        touch(&new_file);
        {
            let mut index = shared.write().unwrap();
            crawl::apply_fs_event(&mut index, &new_file, &patterns);
        }

        let junk = Arc::new(RwLock::new(Arc::new(JunkPatterns::default())));
        let refresh = JunkRefresh::new(shared.clone(), root.to_path_buf(), junk, None);

        refresh.after_fs_burst_for(crate::power::PowerSource::Battery);
        assert_eq!(tier_of(&shared, "from-battery.js"), None);
        assert!(!shared.read().unwrap().junk_dirty.is_empty());

        refresh.after_fs_burst_for(crate::power::PowerSource::External);
        assert_eq!(tier_of(&shared, "from-battery.js"), Some("junk"));
        assert!(shared.read().unwrap().junk_dirty.is_empty());
    }

    // FSEvents delivery is environment- and timing-dependent, so the real watcher is
    // only smoke-covered here and ignored by default. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn fsevents_watcher_smoke() {
        use std::time::{Duration, Instant};

        let dir = TempDir::new();
        // Canonicalize: macOS temp dirs live under the `/var` → `/private/var` symlink,
        // and FSEvents reports canonical paths (matches the init-time canonicalization).
        let root = fs::canonicalize(dir.path()).unwrap();
        let junk = Arc::new(RwLock::new(Arc::new(JunkPatterns::default())));
        let shared = new_index(&root);
        let mapped_dir = TempDir::new();
        persist::save(&shared, &mapped_dir.path().join("watcher-v4.idx"))
            .expect("start watcher from mapped v4 base");
        let (start_tx, start_rx) = mpsc::channel();
        let ready =
            watcher::spawn_deferred(shared.clone(), root.clone(), junk, None, None, start_rx);
        ready
            .recv_timeout(Duration::from_secs(5))
            .expect("FSEvents watcher did not become ready");

        touch(&root.join("live.txt"));
        thread::sleep(Duration::from_millis(400));
        assert_eq!(
            tier_of(&shared, "live.txt"),
            None,
            "deferred watcher applied an event before startup compaction"
        );
        start_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if tier_of(&shared, "live.txt") == Some("normal") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "FSEvents watcher did not index live.txt"
            );
            thread::sleep(Duration::from_millis(50));
        }
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
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.scanned, 0);
        assert!(!response.reused);
    }

    #[test]
    fn existing_typed_path_outside_index_root_skips_scan() {
        // Direct navigation also covers mounted volumes and /private/tmp fixtures that are
        // outside the home-root Name Index. Filesystem existence is sufficient; no indexed
        // slot is required.
        let root_dir = TempDir::new();
        let outside_dir = TempDir::new();
        let typed = outside_dir.path().join("target.txt");
        touch(&typed);

        let root = fs::canonicalize(root_dir.path()).unwrap();
        let shared = new_index(&root);
        let (_journal_dir, journal) = empty_journal();
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
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.scanned, 0);

        let direct = direct_existing_path_response(
            &shared,
            &JunkPatterns::default(),
            &root,
            &typed.to_string_lossy(),
            50,
            Instant::now(),
        )
        .expect("existing external path should bypass the blocking pool");
        assert_eq!(direct.hits[0].path, typed.to_string_lossy());
        assert_eq!(direct.hits.len(), 1);
        assert_eq!(direct.scanned, 0);
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

    #[test]
    fn reuse_falls_back_across_token_boundary() {
        // Extending a single-token query into a multi-token one changes matching semantics:
        // a multi-token token may match on the PATH, which the single-token cache (name
        // matches only) never captured. can_extend must refuse reuse and run a full scan, so
        // the result equals a cold full scan — proving the superset property is preserved by
        // falling back exactly at the «процед» → «процедура п» boundary (#26).
        let dir = TempDir::new();
        let root = dir.path();
        let shared = new_index(root);
        {
            let mut index = shared.write().unwrap();
            let proc_dir = index.add_dir(0, "процедура", model::Tier::Normal, 0);
            index.add_file(proc_dir, "приемки.txt", model::Tier::Normal);
        }
        let (_journal_dir, journal) = empty_journal();
        let junk = Arc::new(JunkPatterns::default());
        let aliases = AliasDictionary::empty();

        // Single-token «процед» matches only the directory entry, by name.
        let base = execute_search(
            &shared,
            &junk,
            root,
            &journal,
            &aliases,
            None,
            "процед",
            50,
            None,
            None,
            RetrievalSignals::default(),
        );
        assert!(base.outcome.exhaustive);
        let cache = ReuseCache {
            query_lower: "процед".to_owned(),
            revision: base.revision,
            entries: base.outcome.candidate_entries.clone(),
        };

        // The file matches «процедура приемки» only via its path (token1 is its ancestor
        // dir), so a naive reuse of the name-only cache would miss it entirely.
        let cold = execute_search(
            &shared,
            &junk,
            root,
            &journal,
            &aliases,
            None,
            "процедура приемки",
            50,
            None,
            None,
            RetrievalSignals::default(),
        );
        let reused = execute_search(
            &shared,
            &junk,
            root,
            &journal,
            &aliases,
            None,
            "процедура приемки",
            50,
            Some(&cache),
            None,
            RetrievalSignals::default(),
        );

        assert!(!reused.reused, "single→multi must fall back to a full scan");
        assert!(!can_extend("процед", "процедура приемки"));
        assert_eq!(reused.outcome.hits, cold.outcome.hits);
        assert!(cold
            .outcome
            .hits
            .iter()
            .any(|hit| hit.name == "приемки.txt"));
    }

    // Machine-local benchmark over the persisted production index. Ignored by default and
    // gated by explicit paths so ordinary test runs never read user data. This complements
    // the synthetic 5M benchmark below: its regular tree catches algorithmic regressions,
    // while the live index exposes costs caused by the reference machine's real name, path,
    // depth, and tier distribution (#31).
    #[test]
    #[ignore]
    fn live_mapped_subtree_removal_timing() {
        let index_path = std::env::var_os("BEELINE_LIVE_INDEX")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_INDEX to the persisted home.idx path");
        let root = std::env::var_os("BEELINE_LIVE_ROOT")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_ROOT to the indexed home root");
        let target = std::env::var_os("BEELINE_LIVE_REMOVAL")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_REMOVAL to an indexed subtree; disk is not changed");

        let mut index = persist::load(&index_path, &root).expect("load live persisted index");
        let parent_path = target.parent().expect("removal target needs a parent");
        let parent = index
            .resolve_dir(parent_path)
            .expect("removal parent must be indexed");
        let name = target
            .file_name()
            .expect("removal target needs a name")
            .to_string_lossy();
        let started = Instant::now();
        let removed = index.remove_child_count(parent, &name);
        let duration_ms = started.elapsed().as_secs_f64() * 1000.0;

        assert!(removed > 0, "removal target must be indexed");
        assert!(!index.has_child(parent, &name));
        println!(
            "live mapped subtree removal: {removed} slots in {duration_ms:.2} ms from {} total slots",
            index.slot_len()
        );
    }

    #[test]
    #[ignore]
    fn live_rebuild_v4_from_filesystem() {
        let root = std::env::var_os("BEELINE_V4_REBUILD_ROOT")
            .map(PathBuf::from)
            .expect("set BEELINE_V4_REBUILD_ROOT to the filesystem root to crawl");
        let output = std::env::var_os("BEELINE_V4_REBUILD_OUTPUT")
            .map(PathBuf::from)
            .expect("set BEELINE_V4_REBUILD_OUTPUT to a temporary v4 path");
        let shared = new_index(&root);
        let junk = JunkPatterns::default();
        let crawl_started = Instant::now();
        crawl::initial_crawl(&shared, root.clone(), &junk, None);
        let crawl_seconds = crawl_started.elapsed().as_secs_f64();
        let entries = shared.read().unwrap().len();
        let persist_started = Instant::now();
        persist::save(&shared, &output).expect("persist live v4 rebuild");
        let persist_ms = persist_started.elapsed().as_secs_f64() * 1000.0;
        let bytes = fs::metadata(&output).expect("stat live v4").len();
        let index = shared.read().unwrap();
        println!(
            "live v4 rebuild: {} entries / {} dirs, crawl {:.2} s, persist+map {:.2} ms, {:.1} MiB",
            entries,
            index.node_len(),
            crawl_seconds,
            persist_ms,
            bytes as f64 / 1024.0 / 1024.0
        );
    }

    #[test]
    #[ignore]
    fn live_persisted_index_load_timing() {
        use std::time::Instant;

        let index_path = std::env::var_os("BEELINE_LIVE_INDEX")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_INDEX to the persisted home.idx path");
        let root = std::env::var_os("BEELINE_LIVE_ROOT")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_ROOT to the indexed home root");
        let started = Instant::now();
        let index = persist::load(&index_path, &root).expect("load live persisted index");
        println!(
            "loaded {} entries / {} dirs in {:.2} ms",
            index.len(),
            index.node_len(),
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    #[test]
    #[ignore]
    fn live_persisted_index_query_timing() {
        use std::time::Instant;

        let index_path = std::env::var_os("BEELINE_LIVE_INDEX")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_INDEX to the persisted home.idx path");
        let root = std::env::var_os("BEELINE_LIVE_ROOT")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_ROOT to the indexed home root");
        let load_started = Instant::now();
        let index = persist::load(&index_path, &root).expect("load live persisted index");
        let load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
        let cores = query::resolve_shards(index.slot_len());
        let non_ascii = (0..index.slot_len())
            .filter_map(|slot| index.entry(slot))
            .filter(|entry| !entry.name.is_ascii())
            .count();
        let name_bytes: usize = (0..index.slot_len())
            .filter_map(|slot| index.entry(slot))
            .map(|entry| entry.name.len())
            .sum();
        let max_name_bytes = (0..index.slot_len())
            .filter_map(|slot| index.entry(slot))
            .map(|entry| entry.name.len())
            .max()
            .unwrap_or(0);
        let junk = (0..index.slot_len())
            .filter_map(|slot| index.entry(slot))
            .filter(|entry| entry.tier == model::Tier::Junk)
            .count();
        let hidden = (0..index.slot_len())
            .filter_map(|slot| index.entry(slot))
            .filter(|entry| entry.tier == model::Tier::Hidden)
            .count();
        println!(
            "loaded {} entries / {} dirs in {load_ms:.2} ms across {cores} shards; names {:.1} bytes avg / {max_name_bytes} max, non-ASCII {non_ascii}, hidden {hidden}, junk {junk}",
            index.len(),
            index.node_len(),
            name_bytes as f64 / index.len() as f64
        );

        let ctx = RankContext::empty();
        let queries = [
            "g",
            "gh",
            "zzz_no_such_entry_zz",
            "процедура приемки",
            "ghjwtlehf ghbtvrb",
            "kirill macbook",
        ];
        for query in queries {
            // Warm query-derived allocation and CPU caches once, then report five production-
            // shard samples. The median is stable enough for comparing local code changes.
            let _ = query::run_impl(&index, &ctx, query, 50, None, None, cores);
            let mut samples = Vec::new();
            let mut last = None;
            for _ in 0..5 {
                let started = Instant::now();
                let outcome = query::run_impl(&index, &ctx, query, 50, None, None, cores);
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                last = Some(outcome);
            }
            samples.sort_by(f64::total_cmp);
            let outcome = last.expect("five samples");
            println!(
                "q={query:?}: median {:.2} ms, min {:.2}, max {:.2}; {} hits, scanned {}, exhaustive {}",
                samples[2],
                samples[0],
                samples[4],
                outcome.hits.len(),
                outcome.scanned,
                outcome.exhaustive
            );
        }

        for shards in [1, 2, 4, 6, cores] {
            let mut samples = Vec::new();
            for _ in 0..3 {
                let started = Instant::now();
                let _ = query::run_impl(&index, &ctx, "процедура приемки", 50, None, None, shards);
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "cyrillic two-token with {shards:>2} shards: median {:.2} ms, min {:.2}, max {:.2}",
                samples[1], samples[0], samples[2]
            );
        }

        for shards in [1, 2, 4, 6, cores] {
            let mut samples = Vec::new();
            for _ in 0..3 {
                let started = Instant::now();
                let _ = query::run_impl(&index, &ctx, "gh", 50, None, None, shards);
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "common two-char with {shards:>2} shards: median {:.2} ms, min {:.2}, max {:.2}",
                samples[1], samples[0], samples[2]
            );
        }

        let upgraded_dir = TempDir::new();
        let upgraded_path = upgraded_dir.path().join("home-v4.idx");
        let shared = Arc::new(RwLock::new(index));
        persist::save(&shared, &upgraded_path).expect("save upgraded live index");
        drop(shared);
        let upgraded_bytes = fs::metadata(&upgraded_path).expect("stat v4 index").len();
        let upgraded_started = Instant::now();
        let upgraded = persist::load(&upgraded_path, &root).expect("reload v3 live index");
        println!(
            "v4 reload: {} entries / {:.1} MiB in {:.2} ms",
            upgraded.len(),
            upgraded_bytes as f64 / 1024.0 / 1024.0,
            upgraded_started.elapsed().as_secs_f64() * 1000.0
        );
        if let Some(seconds) = std::env::var_os("BEELINE_V4_HOLD_SECS")
            .and_then(|value| value.to_string_lossy().parse::<u64>().ok())
        {
            println!(
                "holding pid {} with mapped v4 for {seconds} seconds",
                std::process::id()
            );
            std::thread::sleep(std::time::Duration::from_secs(seconds));
        }
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
                // Cyrillic slice for the live worst case (a pasted two-token Cyrillic query):
                // every 100th project gets an `приемки` subdir holding a few
                // `процедура_{n}.txt` files. The query «процедура приемки» then lands a bounded
                // hit set (25 dirs × 5 files = 125, well under the 4096 candidate cap) yet
                // still full-scans the whole index to find them — the ancestor-walk worst case.
                if a % 100 == 0 {
                    let priemki = index.add_dir(top, "приемки", model::Tier::Normal, 0);
                    for n in 0..5u32 {
                        index.add_file(priemki, &format!("процедура_{n}.txt"), model::Tier::Normal);
                    }
                }
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

        // First-launch searches can run after the crawl but before the atomic v4 rebuild has
        // swapped the mutable crawl representation for its mapped base. Keep that path under
        // the same latency budget as the steady-state mapping.
        {
            let index = shared.read().unwrap();
            let ctx = RankContext::empty();
            let cores = query::resolve_shards(index.slot_len());
            let started = Instant::now();
            let outcome = query::run_impl(&index, &ctx, "процедура приемки", 50, None, None, cores);
            println!(
                "mutable crawl query: {} hits / {} scanned in {:.2} ms",
                outcome.hits.len(),
                outcome.scanned,
                started.elapsed().as_secs_f64() * 1000.0
            );
        }

        // Rebuild into the production v4 layout before timing. This drops the large mutable
        // crawl representation and makes the benchmark exercise the mapped base.
        let mapped_dir = TempDir::new();
        let mapped_path = mapped_dir.path().join("synthetic-v4.idx");
        persist::save(&shared, &mapped_path).expect("persist synthetic v4");

        let index = shared.read().unwrap();
        let ctx = RankContext::empty();
        let cores = query::resolve_shards(index.slot_len());
        println!(
            "scanning {} entries across {cores} shards\n",
            index.slot_len()
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
        // Two-token AND query (#26): every token is matched independently against the name
        // and (on a name miss) the path, so the scan costs up to ~2 needles/entry. The ASCII
        // fast path must keep this inside the §10 Instant budget (≤50 ms). Here "file_1999"
        // selects the project-1999 files by name and "module2" narrows to that module by its
        // ancestor dir — a full scan that lands a few hundred hits.
        bench("two-token (full scan)", "file_1999 module2");
        // The live worst case this ticket targets: a pasted cold two-token Cyrillic query.
        // "процедура" prefix-matches the file names; "приемки" misses every name and falls to
        // the parent-path check, so before the per-dir mask cache each entry re-walked shared
        // ancestor chains. It full-scans (125 hits < cap) — the direct token set then the
        // layout-corrected one, ~4 path checks per entry across the whole index, now one bit
        // test per check against a mask built once per directory.
        bench("cyrillic two-token (full)", "процедура приемки");
        // The same words typed on the wrong (EN) layout: the RU letters of «процедура приемки»
        // map through the physical keys to this ASCII string (derived via `map_layout` RU→EN).
        // The direct tokens match nothing, so the layout-corrected token set carries the hits.
        bench("wrong-layout two-token", "ghjwtlehf ghbtvrb");

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
