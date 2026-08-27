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

fn record(app: Option<&AppHandle>, event: &str, fields: serde_json::Value) {
    if let Some(app) = app {
        if let Err(error) = app.state::<Telemetry>().record(event, fields) {
            eprintln!("telemetry event {event} failed: {error}");
        }
    }
}

/// Register the recursive watcher immediately, but defer applying its queued events until
/// startup has produced the mapped base. This closes the crawl/watch race without letting a
/// large event subtree compete with the initial crawl for the index write lock.
pub fn spawn_deferred(
    shared: Arc<RwLock<IndexData>>,
    root: PathBuf,
    junk: Arc<RwLock<Arc<JunkPatterns>>>,
    junk_refresh: Option<JunkRefresh>,
    app: Option<AppHandle>,
    start: mpsc::Receiver<()>,
) -> mpsc::Receiver<()> {
    let (ready_tx, ready_rx) = mpsc::channel();
    spawn_inner(
        shared,
        root,
        junk,
        junk_refresh,
        app,
        Some(start),
        Some(ready_tx),
    );
    ready_rx
}

fn spawn_inner(
    shared: Arc<RwLock<IndexData>>,
    root: PathBuf,
    junk: Arc<RwLock<Arc<JunkPatterns>>>,
    junk_refresh: Option<JunkRefresh>,
    app: Option<AppHandle>,
    start: Option<mpsc::Receiver<()>>,
    ready: Option<mpsc::Sender<()>>,
) {
    thread::spawn(move || {
        set_background_qos();

        let (tx, rx) = mpsc::channel();
        let mut watcher = match recommended_watcher(move |result| {
            if let Ok(event) = result {
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
        if let Some(ready) = ready {
            let _ = ready.send(());
        }
        if start.is_some_and(|start| start.recv().is_err()) {
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
            let mut saw_junk = false;
            let started = Instant::now();
            let mut applied_paths = 0usize;
            let mut indexed_subtrees = HashSet::new();
            let mut pruned_indexed_descendants = 0usize;
            let mut added = 0usize;
            let mut removed = 0usize;
            for path in prepared.paths {
                if path
                    .ancestors()
                    .skip(1)
                    .any(|ancestor| indexed_subtrees.contains(ancestor))
                {
                    pruned_indexed_descendants += 1;
                    continue;
                }
                if let Ok(relative) = path.strip_prefix(&root) {
                    saw_junk |= patterns.classify(relative) == Tier::Junk;
                }
                let mut index = shared.write().expect("name index lock poisoned");
                let stats = apply_fs_event(&mut index, &path, &patterns);
                applied_paths += 1;
                if stats.indexed_subtree {
                    indexed_subtrees.insert(path);
                }
                added += stats.added;
                removed += stats.removed;
            }
            let duration = started.elapsed();
            if duration >= TELEMETRY_MIN_DURATION
                || received_paths >= 16
                || prepared.pruned_missing_descendants > 0
            {
                record(
                    app.as_ref(),
                    "index_fs_batch_finished",
                    json!({
                        "received_paths": received_paths,
                        "unique_paths": unique_paths,
                        "applied_paths": applied_paths,
                        "deduplicated_paths": received_paths.saturating_sub(unique_paths),
                        "pruned_missing_descendants": prepared.pruned_missing_descendants,
                        "pruned_indexed_descendants": pruned_indexed_descendants,
                        "outside_root": prepared.outside_root,
                        "added_slots": added,
                        "removed_slots": removed,
                        "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                    }),
                );
            }
            // Ordinary activity elsewhere in the home directory must not reset the Junk
            // quiet window. Only another Junk event extends that one-shot delay.
            if saw_junk {
                if let Some(junk_refresh) = &junk_refresh {
                    junk_refresh.after_fs_burst();
                }
            }
        }
    });
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
}
