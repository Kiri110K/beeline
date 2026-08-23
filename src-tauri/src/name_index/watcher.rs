//! Incremental updates from filesystem events (SPEC §6, §10).
//!
//! Energy: this is strictly event-driven. The debounce loop blocks on `recv()` — zero
//! CPU and zero wakeups while the tree is idle — and only after an event arrives does
//! it briefly poll with a timeout to coalesce the burst. There is no periodic timer,
//! satisfying the §10 rule that the hidden resident does no periodic work.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{mpsc, Arc, RwLock},
    thread,
    time::Duration,
};

use notify::{recommended_watcher, RecursiveMode, Watcher};

use crate::name_index::{
    crawl::{apply_fs_event, set_background_qos},
    junk::JunkPatterns,
    model::IndexData,
};

/// How long to keep coalescing a burst of events before applying it.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Start watching the index root recursively and apply changes incrementally. The
/// watcher and its receiver are owned by the spawned thread, which keeps them alive for
/// the process lifetime.
pub fn spawn(shared: Arc<RwLock<IndexData>>, root: PathBuf, junk: Arc<JunkPatterns>) {
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

        loop {
            // Blocks with zero CPU until the tree changes.
            let first = match rx.recv() {
                Ok(event) => event,
                Err(_) => return, // Sender dropped; watcher is gone.
            };

            let mut changed: HashSet<PathBuf> = first.paths.into_iter().collect();
            // Coalesce the rest of the burst.
            loop {
                match rx.recv_timeout(DEBOUNCE) {
                    Ok(event) => changed.extend(event.paths),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }

            for path in changed {
                let mut index = shared.write().expect("name index lock poisoned");
                apply_fs_event(&mut index, &path, &junk);
            }
        }
    });
}
