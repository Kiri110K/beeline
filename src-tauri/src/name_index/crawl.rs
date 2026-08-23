//! Filesystem-driven index operations: the initial two-phase crawl, incremental
//! apply of a single change, the startup diff-rescan, and the lazy Junk drain.
//!
//! Locking discipline (SPEC §10 energy + responsiveness):
//! - The **initial crawl** never holds the write lock across IO. It reads one
//!   directory (IO, no lock), then takes the lock only to insert that directory's
//!   children as a batch. Search stays responsive on partial data throughout.
//! - The incremental apply, diff-rescan, and Junk drain each hold the write lock for
//!   a single bounded filesystem operation (one directory). These run in bursts on
//!   small working sets, so per-directory lock holds are acceptable and keep the code
//!   simple.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Instant, UNIX_EPOCH},
};

use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::{
    name_index::{
        junk::JunkPatterns,
        model::{DirId, IndexData, Tier},
    },
    telemetry::Telemetry,
};

/// Progress telemetry cadence: one event per this many indexed entries.
const PROGRESS_STEP: usize = 100_000;

/// Lower the calling thread's QoS on macOS so index work never competes with the
/// UI. This links `libSystem` (always present); no crate dependency is needed.
/// `QOS_CLASS_BACKGROUND` is `0x09`, `QOS_CLASS_UTILITY` is `0x11`.
fn set_qos(qos_class: u32) {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `pthread_set_qos_class_self_np` is a libSystem C function that only
        // reads its two scalar arguments and adjusts the current thread's QoS.
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        // SAFETY: see above — plain scalar call, no memory is touched.
        unsafe {
            pthread_set_qos_class_self_np(qos_class, 0);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = qos_class;
}

/// Steady-state index work (FSEvents increments, junk drains) stays at background
/// QoS: rare small bursts where throttling costs nothing.
pub fn set_background_qos() {
    set_qos(0x09);
}

/// The one-time initial crawl and the startup diff-rescan run at utility QoS.
/// Background QoS gets darwin's heaviest IO throttle — measured on the reference
/// machine it starved the crawl to ~84 entries/s in `opendir` (10+ hours for a
/// full home). Deviation from SPEC §10's blanket "background QoS for heavy work"
/// is recorded on ticket #26.
pub fn set_crawl_qos() {
    set_qos(0x11);
}

fn mtime_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

fn record(app: Option<&AppHandle>, event: &str, fields: serde_json::Value) {
    if let Some(app) = app {
        if let Err(error) = app.state::<Telemetry>().record(event, fields) {
            eprintln!("telemetry event {event} failed: {error}");
        }
    }
}

/// One child read from disk, before it is inserted under the lock.
struct ChildInfo {
    name: String,
    is_dir: bool,
    tier: Tier,
    mtime: i64,
    path: PathBuf,
}

/// Read one directory's direct children off disk (no lock held). Symlinks are
/// recorded as non-directory entries and never descended, which avoids cycles.
fn read_children(path: &Path, root: &Path, junk: &JunkPatterns) -> Vec<ChildInfo> {
    let mut children = Vec::new();
    let Ok(read_dir) = fs::read_dir(path) else {
        return children;
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_path = entry.path();
        let is_dir = entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false);
        let tier = match child_path.strip_prefix(root) {
            Ok(relative) => junk.classify(relative),
            Err(_) => Tier::Normal,
        };
        let mtime = if is_dir {
            entry.metadata().map(|meta| mtime_ms(&meta)).unwrap_or(0)
        } else {
            0
        };
        children.push(ChildInfo {
            name,
            is_dir,
            tier,
            mtime,
            path: child_path,
        });
    }
    children
}

/// Breadth-first crawl starting at directory `start` (which must already exist in the
/// index). With `two_phase`, non-Junk content is fully indexed before Junk subtrees
/// (SPEC §6: Junk is crawled, but after everything else). Returns the number of
/// entries added.
fn crawl_walk(
    shared: &Arc<RwLock<IndexData>>,
    start: DirId,
    start_path: PathBuf,
    root: &Path,
    junk: &JunkPatterns,
    app: Option<&AppHandle>,
    two_phase: bool,
) -> usize {
    let mut normal: VecDeque<(DirId, PathBuf)> = VecDeque::new();
    let mut deferred_junk: Vec<(DirId, PathBuf)> = Vec::new();
    normal.push_back((start, start_path));

    let mut added = 0usize;
    let mut last_progress = 0usize;

    // Phase 1 drains `normal`; any Junk subtree encountered is deferred. Phase 2 then
    // drains what was deferred (only when `two_phase`).
    loop {
        while let Some((dir_id, dir_path)) = normal.pop_front() {
            let children = read_children(&dir_path, root, junk);
            if children.is_empty() {
                continue;
            }
            let mut index = shared.write().expect("name index lock poisoned");
            for child in children {
                if child.is_dir {
                    let id = index.add_dir(dir_id, &child.name, child.tier, child.mtime);
                    added += 1;
                    if two_phase && child.tier == Tier::Junk {
                        deferred_junk.push((id, child.path));
                    } else {
                        normal.push_back((id, child.path));
                    }
                } else {
                    index.add_file(dir_id, &child.name, child.tier);
                    added += 1;
                }
            }
            drop(index);

            if added - last_progress >= PROGRESS_STEP {
                last_progress = added;
                record(app, "index_crawl_progress", json!({ "entries": added }));
            }
        }

        match deferred_junk.pop() {
            Some(entry) => normal.push_back(entry),
            None => break,
        }
    }

    added
}

/// Run the one-time full crawl of the index root, reporting telemetry throughout.
pub fn initial_crawl(
    shared: &Arc<RwLock<IndexData>>,
    root: PathBuf,
    junk: &JunkPatterns,
    app: Option<&AppHandle>,
) {
    record(app, "index_crawl_started", json!({}));
    let started = Instant::now();
    let added = crawl_walk(shared, 0, root.clone(), &root, junk, app, true);
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let entries = shared.read().expect("name index lock poisoned").len();
    record(
        app,
        "index_crawl_finished",
        json!({ "entries": entries, "duration_ms": duration_ms, "added": added }),
    );
}

/// Index a subtree rooted at an already-existing directory node, operating directly on
/// a borrowed [`IndexData`] (used by the incremental apply, which is already inside a
/// single directory's fs read). Single-phase: Junk is indexed inline.
fn index_subtree_into(data: &mut IndexData, dir_id: DirId, path: &Path, junk: &JunkPatterns) {
    let root = data.root.clone();
    for child in read_children(path, &root, junk) {
        if child.is_dir {
            let id = data.add_dir(dir_id, &child.name, child.tier, child.mtime);
            index_subtree_into(data, id, &child.path, junk);
        } else {
            data.add_file(dir_id, &child.name, child.tier);
        }
    }
}

/// Apply a single filesystem change at `path` to the index. Junk paths are not
/// rescanned; the containing directory is marked dirty for a later lazy refresh
/// (SPEC §6). Non-Junk changes are applied immediately.
pub fn apply_fs_event(data: &mut IndexData, path: &Path, junk: &JunkPatterns) {
    let Ok(relative) = path.strip_prefix(&data.root) else {
        return;
    };
    let tier = junk.classify(relative);
    let Some(parent_path) = path.parent() else {
        return;
    };

    if tier == Tier::Junk {
        if let Some(parent) = data.resolve_dir(parent_path) {
            data.junk_dirty.insert(parent);
        }
        return;
    }

    let Some(parent) = data.resolve_dir(parent_path) else {
        return;
    };
    let Some(name) = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return;
    };

    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                let id = data.add_dir(parent, &name, tier, mtime_ms(&metadata));
                index_subtree_into(data, id, path, junk);
            } else if !data.has_child(parent, &name) {
                data.add_file(parent, &name, tier);
            }
        }
        Err(_) => {
            data.remove_child(parent, &name);
        }
    }
}

/// Reconcile one directory's direct children with disk: add appeared items, remove
/// vanished ones, refresh the stored mtime, and crawl any newly-appeared subdirs.
fn reconcile_dir(
    shared: &Arc<RwLock<IndexData>>,
    dir_id: DirId,
    path: &Path,
    root: &Path,
    junk: &JunkPatterns,
) {
    let Ok(dir_metadata) = fs::metadata(path) else {
        return;
    };
    let disk_mtime = mtime_ms(&dir_metadata);
    let disk: HashMap<String, ChildInfo> = read_children(path, root, junk)
        .into_iter()
        .map(|child| (child.name.clone(), child))
        .collect();

    // Snapshot current index children (names + whether directory).
    let index_children: Vec<(String, bool)> = {
        let index = shared.read().expect("name index lock poisoned");
        index.nodes[dir_id as usize]
            .entries
            .iter()
            .filter_map(|&entry_index| {
                index.entries[entry_index as usize]
                    .as_ref()
                    .map(|entry| (entry.name.to_string(), entry.is_directory))
            })
            .collect()
    };

    let mut new_subdirs: Vec<(DirId, PathBuf)> = Vec::new();
    {
        let mut index = shared.write().expect("name index lock poisoned");
        for (name, _) in &index_children {
            if !disk.contains_key(name) {
                index.remove_child(dir_id, name);
            }
        }
        let known: std::collections::HashSet<&str> = index_children
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        for child in disk.values() {
            if known.contains(child.name.as_str()) {
                continue;
            }
            if child.is_dir {
                let id = index.add_dir(dir_id, &child.name, child.tier, child.mtime);
                new_subdirs.push((id, child.path.clone()));
            } else {
                index.add_file(dir_id, &child.name, child.tier);
            }
        }
        index.nodes[dir_id as usize].mtime_ms = disk_mtime;
    }

    for (id, subdir_path) in new_subdirs {
        crawl_walk(shared, id, subdir_path, root, junk, None, false);
    }
}

/// Reconcile the whole tree against disk on startup, skipping directories whose mtime
/// is unchanged and skipping Junk subtrees entirely (lazy, SPEC §6). This is cheaper
/// than a full rebuild: unchanged directories are never re-read.
///
/// mtime only reflects direct-child changes, so deep edits made while the app was
/// closed under an mtime-unchanged parent are caught by recursion, not by the parent's
/// mtime. A fully reliable catch-up would rehash contents; that is out of scope here.
pub fn diff_rescan(
    shared: &Arc<RwLock<IndexData>>,
    root: PathBuf,
    junk: &JunkPatterns,
    app: Option<&AppHandle>,
) {
    let started = Instant::now();
    diff_rescan_dir(shared, 0, root.clone(), &root, junk);
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let entries = shared.read().expect("name index lock poisoned").len();
    record(
        app,
        "index_diff_rescan_finished",
        json!({ "entries": entries, "duration_ms": duration_ms }),
    );
}

fn diff_rescan_dir(
    shared: &Arc<RwLock<IndexData>>,
    dir_id: DirId,
    path: PathBuf,
    root: &Path,
    junk: &JunkPatterns,
) {
    let (stored_mtime, tier) = {
        let index = shared.read().expect("name index lock poisoned");
        let node = &index.nodes[dir_id as usize];
        (node.mtime_ms, node.tier)
    };
    if tier == Tier::Junk {
        return; // Lazy: Junk is refreshed on a targeting query, not on startup.
    }

    let disk_mtime = fs::metadata(&path).map(|meta| mtime_ms(&meta)).ok();
    match disk_mtime {
        Some(disk_mtime) if disk_mtime != stored_mtime => {
            reconcile_dir(shared, dir_id, &path, root, junk);
        }
        Some(_) => {}
        None => return, // Directory is gone; its parent's reconcile removes it.
    }

    let children: Vec<(String, DirId)> = {
        let index = shared.read().expect("name index lock poisoned");
        index.nodes[dir_id as usize]
            .child_dirs
            .iter()
            .map(|(name, &id)| (name.to_string(), id))
            .collect()
    };
    for (name, id) in children {
        diff_rescan_dir(shared, id, path.join(name), root, junk);
    }
}

/// Rescan every dirty Junk directory, then clear the dirty set. Called when a query
/// targets Junk (SPEC §6). Each dirty directory is emptied and re-crawled fresh.
pub fn drain_junk_dirty(shared: &Arc<RwLock<IndexData>>, root: &Path, junk: &JunkPatterns) {
    let dirty: Vec<(DirId, PathBuf)> = {
        let mut index = shared.write().expect("name index lock poisoned");
        let ids: Vec<DirId> = index.junk_dirty.drain().collect();
        ids.into_iter()
            .map(|id| (id, index.full_path(id)))
            .collect()
    };
    for (id, path) in dirty {
        {
            let mut index = shared.write().expect("name index lock poisoned");
            index.clear_children(id);
        }
        crawl_walk(shared, id, path, root, junk, None, false);
    }
}
