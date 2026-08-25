//! Incremental updates from filesystem events (SPEC §6, §10).
//!
//! Energy: this is strictly event-driven. The debounce loop blocks on `recv()` — zero
//! CPU and zero wakeups while the tree is idle — and only after an event arrives does
//! it collect a fixed, short batch. The deadline is fixed rather than reset per event,
//! so continuous filesystem traffic can never starve index updates. There is no periodic
//! timer, satisfying the §10 rule that the hidden resident does no periodic work.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{mpsc, Arc, RwLock},
    thread,
    time::{Duration, Instant},
};

use notify::{recommended_watcher, RecursiveMode, Watcher};

use crate::name_index::{
    crawl::{apply_fs_event, set_background_qos},
    junk::JunkPatterns,
    junk_refresh::JunkRefresh,
    model::{IndexData, Tier},
};

/// Maximum time to collect one batch of filesystem events before applying it.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Register the recursive watcher immediately, but defer applying its queued events until
/// startup has produced the mapped base. This closes the crawl/watch race without letting a
/// large event subtree compete with the initial crawl for the index write lock.
pub fn spawn_deferred(
    shared: Arc<RwLock<IndexData>>,
    root: PathBuf,
    junk: Arc<RwLock<Arc<JunkPatterns>>>,
    junk_refresh: Option<JunkRefresh>,
    start: mpsc::Receiver<()>,
) -> mpsc::Receiver<()> {
    let (ready_tx, ready_rx) = mpsc::channel();
    spawn_inner(
        shared,
        root,
        junk,
        junk_refresh,
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
                    Ok(event) => changed.extend(event.paths),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }

            // Snapshot the current Junk patterns once per burst so a live Settings change
            // classifies newly-seen paths without re-locking per path.
            let patterns = junk.read().expect("junk lock poisoned").clone();
            let mut saw_junk = false;
            for path in changed {
                if let Ok(relative) = path.strip_prefix(&root) {
                    saw_junk |= patterns.classify(relative) == Tier::Junk;
                }
                let mut index = shared.write().expect("name index lock poisoned");
                apply_fs_event(&mut index, &path, &patterns);
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
