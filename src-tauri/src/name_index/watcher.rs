//! Incremental updates from filesystem events (SPEC §6, §10).
//!
//! Energy: this is strictly event-driven. The debounce loop blocks on `recv()` — zero
//! CPU and zero wakeups while the tree is idle — and only after an event arrives does
//! it collect a fixed, short batch. The deadline is fixed rather than reset per event,
//! so continuous filesystem traffic can never starve index updates. There is no periodic
//! timer, satisfying the §10 rule that the hidden resident does no periodic work.

use std::{
    collections::HashSet,
    fs,
    path::Path,
    path::PathBuf,
    sync::{mpsc, Arc, RwLock},
    thread,
    time::{Duration, Instant},
};

use notify::{recommended_watcher, RecursiveMode, Watcher};
use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::{
    name_index::{
        crawl::{apply_fs_event, set_background_qos},
        junk::JunkPatterns,
        junk_refresh::JunkRefresh,
        model::{IndexData, Tier},
        overlay_journal::OverlayJournal,
    },
    telemetry::Telemetry,
};

/// Maximum time to collect one batch of filesystem events before applying it.
const DEBOUNCE: Duration = Duration::from_millis(200);
const TELEMETRY_MIN_DURATION: Duration = Duration::from_millis(25);

struct PreparedPaths {
    paths: Vec<PathBuf>,
    pruned_missing_descendants: usize,
    outside_root: usize,
}

struct AppliedPaths {
    applied_paths: usize,
    pruned_indexed_descendants: usize,
    added: usize,
    removed: usize,
    saw_junk: bool,
}

fn prepare_paths_with(
    root: &Path,
    changed: HashSet<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> PreparedPaths {
    let mut candidates: Vec<PathBuf> = changed.into_iter().collect();
    // Missing ancestors must run before their descendants. Removing the ancestor consumes
    // its complete indexed subtree, so every descendant event in the same burst is stale.
    candidates.sort_unstable_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });

    let mut paths = Vec::with_capacity(candidates.len());
    let mut missing = HashSet::new();
    let mut pruned_missing_descendants = 0usize;
    let mut outside_root = 0usize;

    for path in candidates {
        if path != root && !path.starts_with(root) {
            outside_root += 1;
            continue;
        }
        let covered = path
            .ancestors()
            .skip(1)
            .any(|ancestor| ancestor.starts_with(root) && missing.contains(ancestor));
        if covered {
            pruned_missing_descendants += 1;
            continue;
        }
        if !exists(&path) {
            missing.insert(path.clone());
        }
        paths.push(path);
    }

    PreparedPaths {
        paths,
        pruned_missing_descendants,
        outside_root,
    }
}

fn prepare_paths(root: &Path, changed: HashSet<PathBuf>) -> PreparedPaths {
    prepare_paths_with(root, changed, |path| fs::symlink_metadata(path).is_ok())
}

fn retain_external_paths(paths: &mut Vec<PathBuf>, ignored_subtree: Option<&Path>) {
    if let Some(ignored) = ignored_subtree {
        paths.retain(|path| !path.starts_with(ignored));
    }
}

fn record(app: Option<&AppHandle>, event: &str, fields: serde_json::Value) {
    if let Some(app) = app {
        if let Err(error) = app.state::<Telemetry>().record(event, fields) {
            eprintln!("telemetry event {event} failed: {error}");
        }
    }
}

#[derive(Default)]
pub struct SideEffects {
    pub junk_refresh: Option<JunkRefresh>,
    pub journal: Option<OverlayJournal>,
    pub app: Option<AppHandle>,
}

/// Register the recursive watcher immediately, but defer applying its queued events until
/// startup has produced the mapped base. This closes the crawl/watch race without letting a
/// large event subtree compete with the initial crawl for the index write lock.
pub fn spawn_deferred(
    shared: Arc<RwLock<IndexData>>,
    root: PathBuf,
    ignored_subtree: Option<PathBuf>,
    junk: Arc<RwLock<Arc<JunkPatterns>>>,
    effects: SideEffects,
    start: mpsc::Receiver<()>,
) -> mpsc::Receiver<()> {
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::spawn(move || {
        set_background_qos();

        let (tx, rx) = mpsc::channel();
        let mut watcher = match recommended_watcher(move |result: notify::Result<notify::Event>| {
            if let Ok(mut event) = result {
                retain_external_paths(&mut event.paths, ignored_subtree.as_deref());
                if event.paths.is_empty() {
                    return;
                }
                let _ = tx.send(event);
            }
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("name index watcher creation failed: {error}");
                return;
            }
        };
        if let Err(error) = watcher.watch(&root, RecursiveMode::Recursive) {
            eprintln!("name index watch failed: {error}");
            return;
        }
        let _ = ready_tx.send(());
        if start.recv().is_err() {
            return;
        }

        loop {
            // Blocks with zero CPU until the tree changes.
            let first = match rx.recv() {
                Ok(event) => event,
                Err(_) => return, // Sender dropped; watcher is gone.
            };

            let mut received_paths = first.paths.len();
            let mut changed: HashSet<PathBuf> = first.paths.into_iter().collect();
            // Coalesce for at most one fixed window. Resetting the timeout after every
            // event would starve forever on a busy home directory, exactly when live index
            // updates matter most.
            let deadline = Instant::now() + DEBOUNCE;
            loop {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                match rx.recv_timeout(remaining) {
                    Ok(event) => {
                        received_paths += event.paths.len();
                        changed.extend(event.paths);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }

            // Snapshot the current Junk patterns once per burst so a live Settings change
            // classifies newly-seen paths without re-locking per path.
            let unique_paths = changed.len();
            let prepared = prepare_paths(&root, changed);
            let patterns = junk.read().expect("junk lock poisoned").clone();
            if let Some(journal) = &effects.journal {
                if let Err(error) =
                    journal.record_paths(prepared.paths.iter().map(PathBuf::as_path))
                {
                    record(
                        effects.app.as_ref(),
                        "index_overlay_journal_failed",
                        json!({ "phase": "append", "error": error.to_string() }),
                    );
                }
            }
            let started = Instant::now();
            let pruned_missing_descendants = prepared.pruned_missing_descendants;
            let outside_root = prepared.outside_root;
            let applied = apply_prepared_paths(&shared, &root, &patterns, prepared);
            let duration = started.elapsed();
            if duration >= TELEMETRY_MIN_DURATION
                || received_paths >= 16
                || pruned_missing_descendants > 0
            {
                record(
                    effects.app.as_ref(),
                    "index_fs_batch_finished",
                    json!({
                        "received_paths": received_paths,
                        "unique_paths": unique_paths,
                        "applied_paths": applied.applied_paths,
                        "deduplicated_paths": received_paths.saturating_sub(unique_paths),
                        "pruned_missing_descendants": pruned_missing_descendants,
                        "pruned_indexed_descendants": applied.pruned_indexed_descendants,
                        "outside_root": outside_root,
                        "added_slots": applied.added,
                        "removed_slots": applied.removed,
                        "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                    }),
                );
            }
            // Ordinary activity elsewhere in the home directory must not reset the Junk
            // quiet window. Only another Junk event extends that one-shot delay.
            if applied.saw_junk {
                if let Some(junk_refresh) = &effects.junk_refresh {
                    junk_refresh.after_fs_burst();
                }
            }
        }
    });
    ready_rx
}

fn apply_prepared_paths(
    shared: &Arc<RwLock<IndexData>>,
    root: &Path,
    patterns: &JunkPatterns,
    prepared: PreparedPaths,
) -> AppliedPaths {
    let mut applied_paths = 0usize;
    let mut indexed_subtrees = HashSet::new();
    let mut pruned_indexed_descendants = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut saw_junk = false;
    for path in prepared.paths {
        if path
            .ancestors()
            .skip(1)
            .any(|ancestor| indexed_subtrees.contains(ancestor))
        {
            pruned_indexed_descendants += 1;
            continue;
        }
        if let Ok(relative) = path.strip_prefix(root) {
            saw_junk |= patterns.classify(relative) == Tier::Junk;
        }
        let mut index = shared.write().expect("name index lock poisoned");
        let stats = apply_fs_event(&mut index, &path, patterns);
        applied_paths += 1;
        if stats.indexed_subtree {
            indexed_subtrees.insert(path);
        }
        added += stats.added;
        removed += stats.removed;
    }
    AppliedPaths {
        applied_paths,
        pruned_indexed_descendants,
        added,
        removed,
        saw_junk,
    }
}

pub fn replay_paths(
    shared: &Arc<RwLock<IndexData>>,
    root: &Path,
    patterns: &JunkPatterns,
    paths: Vec<PathBuf>,
) -> (usize, usize, usize) {
    let prepared = prepare_paths(root, paths.into_iter().collect());
    let applied = apply_prepared_paths(shared, root, patterns, prepared);
    (applied.applied_paths, applied.added, applied.removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ancestor_prunes_all_descendant_events() {
        let root = PathBuf::from("/root");
        let gone = root.join("target/debug/build");
        let changed = HashSet::from([
            gone.join("crate-a/out/file.rs"),
            root.join("target"),
            gone.join("crate-a"),
            gone.clone(),
            root.join("target/debug/kept"),
        ]);

        let prepared = prepare_paths_with(&root, changed, |path| path != gone);
        assert_eq!(prepared.pruned_missing_descendants, 2);
        assert!(prepared.paths.contains(&gone));
        assert!(prepared.paths.contains(&root.join("target/debug/kept")));
        assert!(!prepared.paths.contains(&gone.join("crate-a")));
    }

    #[test]
    fn existing_ancestor_keeps_deeper_directory_events() {
        let root = PathBuf::from("/root");
        let changed = HashSet::from([
            root.join("target"),
            root.join("target/debug"),
            root.join("target/debug/build"),
        ]);

        let prepared = prepare_paths_with(&root, changed, |_| true);
        assert_eq!(prepared.paths.len(), 3);
        assert_eq!(prepared.pruned_missing_descendants, 0);
    }

    #[test]
    fn internal_app_data_is_filtered_without_hiding_siblings() {
        let root = PathBuf::from("/Users/test");
        let ignored = root.join("Library/Application Support/com.beeline.app");
        let mut paths = vec![
            ignored.join("telemetry.ndjson"),
            ignored.join("settings.json"),
            root.join("Library/Application Support/another-app/data.json"),
            root.join("work/project/readme.md"),
        ];

        retain_external_paths(&mut paths, Some(&ignored));

        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&root.join("Library/Application Support/another-app/data.json")));
        assert!(paths.contains(&root.join("work/project/readme.md")));
    }
}
