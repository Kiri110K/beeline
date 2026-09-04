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
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    thread,
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
/// Bound deferred exact Junk paths. If a single quiet-window burst exceeds this, the drain
/// falls back to the older per-directory reconcile without retaining an unbounded path set.
const MAX_PENDING_JUNK_PATHS: usize = 100_000;
/// Listing one directory wins once a burst changed many of its direct children. Below this count,
/// exact `stat`/apply avoids cloning and hashing every untouched sibling. The value is deliberately
/// conservative; the production 90k-child reference directory crosses over far below it.
const MAX_EXACT_JUNK_PATHS_PER_DIR: usize = 256;

/// Steady-state index work (FSEvents increments, junk drains) stays at background
/// QoS: rare small bursts where throttling costs nothing. The libSystem shim lives in
/// [`crate::qos`] so the operations engine reuses it.
pub use crate::qos::set_background_qos;

/// The one-time initial crawl and the startup diff-rescan run at utility QoS.
/// Background QoS gets darwin's heaviest IO throttle — measured on the reference
/// machine it starved the crawl to ~84 entries/s in `opendir` (10+ hours for a
/// full home). Deviation from SPEC §10's blanket "background QoS for heavy work"
/// is recorded on ticket #26.
pub fn set_crawl_qos() {
    crate::qos::set_qos(0x11);
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FsApplyStats {
    pub added: usize,
    pub removed: usize,
    /// The event inserted this path as a fresh directory and crawled its whole subtree.
    /// Later descendant paths from the same FSEvents burst are therefore redundant.
    pub indexed_subtree: bool,
}

impl FsApplyStats {
    fn combine(&mut self, other: Self) {
        self.added += other.added;
        self.removed += other.removed;
    }
}

/// Read one directory's direct children off disk (no lock held). Symlinks are
/// recorded as non-directory entries and never descended, which avoids cycles.
fn read_children(path: &Path, root: &Path, junk: &JunkPatterns) -> std::io::Result<Vec<ChildInfo>> {
    let mut children = Vec::new();
    let read_dir = fs::read_dir(path)?;
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
    Ok(children)
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
            let Ok(children) = read_children(&dir_path, root, junk) else {
                continue;
            };
            if children.is_empty() {
                continue;
            }
            let mut index = shared.write().expect("name index lock poisoned");
            for child in children {
                if child.is_dir {
                    // Every directory reached by crawl_walk is freshly inserted and empty.
                    // Rechecking all siblings here made wide new subtrees quadratic.
                    let id =
                        index.add_dir_known_absent(dir_id, &child.name, child.tier, child.mtime);
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
///
/// Invariant: `dir_id` must be a freshly created, empty directory node. Every file is
/// added with an unchecked `add_file`, so running this over an already-populated
/// directory duplicates its whole subtree. An FSEvent on an existing dir reconciles
/// direct children instead (see [`reconcile_dir_children`]).
fn index_subtree_into(
    data: &mut IndexData,
    dir_id: DirId,
    path: &Path,
    junk: &JunkPatterns,
) -> usize {
    let root = data.root.clone();
    let mut added = 0usize;
    let Ok(children) = read_children(path, &root, junk) else {
        return 0;
    };
    for child in children {
        if child.is_dir {
            let id = data.add_dir_known_absent(dir_id, &child.name, child.tier, child.mtime);
            added += 1 + index_subtree_into(data, id, &child.path, junk);
        } else {
            data.add_file(dir_id, &child.name, child.tier);
            added += 1;
        }
    }
    added
}

/// Apply a single filesystem change at `path` to the index. Junk paths are retained for a
/// later exact-path drain and their containing directory is kept as the bounded-overflow
/// fallback (SPEC §6). Non-Junk changes are applied immediately.
pub fn apply_fs_event(data: &mut IndexData, path: &Path, junk: &JunkPatterns) -> FsApplyStats {
    apply_fs_event_with_junk_policy(data, path, junk, true)
}

fn apply_fs_event_with_junk_policy(
    data: &mut IndexData,
    path: &Path,
    junk: &JunkPatterns,
    defer_junk: bool,
) -> FsApplyStats {
    if path == data.root {
        let root = data.root.clone();
        let mtime = fs::metadata(path)
            .map(|metadata| mtime_ms(&metadata))
            .unwrap_or(0);
        return reconcile_dir_children(data, 0, &root, mtime, junk);
    }
    let Ok(relative) = path.strip_prefix(&data.root) else {
        return FsApplyStats::default();
    };
    let tier = junk.classify(relative);
    let Some(parent_path) = path.parent() else {
        return FsApplyStats::default();
    };

    if tier == Tier::Junk && defer_junk {
        if !data.junk_paths_saturated {
            data.junk_changed_paths.insert(path.to_path_buf());
            if data.junk_changed_paths.len() > MAX_PENDING_JUNK_PATHS {
                data.junk_changed_paths.clear();
                data.junk_paths_saturated = true;
            }
        }
        if let Some(parent) = data.resolve_dir(parent_path) {
            data.junk_dirty.insert(parent);
            refresh_dir_mtime(data, parent, parent_path);
        }
        return FsApplyStats::default();
    }

    let Some(parent) = data.resolve_dir(parent_path) else {
        return FsApplyStats::default();
    };
    let Some(name) = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return FsApplyStats::default();
    };

    let stats = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                // FSEvents on a dir means its direct children changed. Reconcile an
                // already-indexed dir in place; re-running index_subtree_into (unchecked
                // add_file) would duplicate the whole subtree on every event.
                let existing = data.child_slot(parent, &name).and_then(|slot| {
                    data.entry(slot as usize)
                        .map(|entry| (entry.is_directory, entry.dir_id))
                });
                match existing {
                    Some((true, dir_id)) => {
                        reconcile_dir_children(data, dir_id, path, mtime_ms(&metadata), junk)
                    }
                    existing => {
                        let removed = if existing.is_some() {
                            data.remove_child_count(parent, &name)
                        } else {
                            0
                        };
                        let id =
                            data.add_dir_known_absent(parent, &name, tier, mtime_ms(&metadata));
                        FsApplyStats {
                            added: 1 + index_subtree_into(data, id, path, junk),
                            removed,
                            indexed_subtree: true,
                        }
                    }
                }
            } else {
                let existing = data
                    .child_slot(parent, &name)
                    .and_then(|slot| data.entry(slot as usize).map(|entry| entry.is_directory));
                match existing {
                    Some(false) => FsApplyStats::default(),
                    existing => {
                        let removed = if existing.is_some() {
                            data.remove_child_count(parent, &name)
                        } else {
                            0
                        };
                        data.add_file(parent, &name, tier);
                        FsApplyStats {
                            added: 1,
                            removed,
                            indexed_subtree: false,
                        }
                    }
                }
            }
        }
        Err(_) => FsApplyStats {
            added: 0,
            removed: data.remove_child_count(parent, &name),
            indexed_subtree: false,
        },
    };
    // Directory mtimes represent changes to their direct children. FSEvents commonly
    // reports only the child path; without refreshing its parent, every restart re-reads
    // that directory even though the journal already reconstructed the change.
    refresh_dir_mtime(data, parent, parent_path);
    stats
}

fn refresh_dir_mtime(data: &mut IndexData, dir_id: DirId, path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        data.set_node_mtime(dir_id, mtime_ms(&metadata));
    }
}

/// Reconcile one already-indexed directory's DIRECT children against disk, in place on a
/// borrowed [`IndexData`]. Scope is direct children only: an FSEvent on a directory means
/// its own children changed, and deeper changes arrive as their own events. Index children
/// gone from disk are removed; disk children missing from the index are added (a fresh
/// subdir's subtree is genuinely new, so it is crawled via [`index_subtree_into`]); the
/// stored mtime is refreshed.
fn reconcile_dir_children(
    data: &mut IndexData,
    dir_id: DirId,
    path: &Path,
    mtime: i64,
    junk: &JunkPatterns,
) -> FsApplyStats {
    let root = data.root.clone();
    // A transient permission or IO failure is not evidence that the directory became
    // empty. Keep the indexed children and let a later event or startup diff retry.
    let Ok(disk) = read_children(path, &root, junk) else {
        return FsApplyStats::default();
    };
    let on_disk: HashSet<&str> = disk.iter().map(|child| child.name.as_str()).collect();
    let mut stats = FsApplyStats::default();

    // Drop index children no longer present on disk.
    let indexed: Vec<String> = data
        .direct_children(dir_id)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for name in &indexed {
        if !on_disk.contains(name.as_str()) {
            stats.removed += data.remove_child_count(dir_id, name);
        }
    }

    // Add disk children absent from the post-removal index. A HashSet gives O(1)
    // membership instead of an O(children) `has_child` probe per candidate.
    let known: HashSet<String> = data
        .direct_children(dir_id)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for child in disk {
        if known.contains(&child.name) {
            continue;
        }
        if child.is_dir {
            let id = data.add_dir_known_absent(dir_id, &child.name, child.tier, child.mtime);
            stats.combine(FsApplyStats {
                added: 1 + index_subtree_into(data, id, &child.path, junk),
                removed: 0,
                indexed_subtree: false,
            });
        } else {
            data.add_file(dir_id, &child.name, child.tier);
            stats.added += 1;
        }
    }

    data.set_node_mtime(dir_id, mtime);
    stats
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
    let Ok(children) = read_children(path, root, junk) else {
        return;
    };
    let disk: HashMap<String, ChildInfo> = children
        .into_iter()
        .map(|child| (child.name.clone(), child))
        .collect();

    // Snapshot current index children (names + whether directory).
    let index_children: Vec<(String, bool)> = {
        let index = shared.read().expect("name index lock poisoned");
        index.direct_children(dir_id)
    };

    let mut new_subdirs: Vec<(DirId, PathBuf)> = Vec::new();
    {
        let mut index = shared.write().expect("name index lock poisoned");
        for (name, is_directory) in &index_children {
            if disk
                .get(name)
                .is_none_or(|child| child.is_dir != *is_directory)
            {
                index.remove_child(dir_id, name);
            }
        }
        let known: HashSet<String> = index
            .direct_children(dir_id)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for child in disk.values() {
            if known.contains(&child.name) {
                continue;
            }
            if child.is_dir {
                let id = index.add_dir_known_absent(dir_id, &child.name, child.tier, child.mtime);
                new_subdirs.push((id, child.path.clone()));
            } else {
                index.add_file(dir_id, &child.name, child.tier);
            }
        }
        index.set_node_mtime(dir_id, disk_mtime);
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
    let _activity = crate::qos::begin_bounded_startup_activity("Beeline startup index diff");
    let started = Instant::now();
    let stats = diff_rescan_tree(shared, root, junk);
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let entries = shared.read().expect("name index lock poisoned").len();
    record(
        app,
        "index_diff_rescan_finished",
        json!({
            "entries": entries,
            "duration_ms": duration_ms,
            "visited_dirs": stats.visited,
            "reconciled_dirs": stats.reconciled,
            "missing_dirs": stats.missing,
            "junk_dirs": stats.junk,
            "snapshot_ms": stats.snapshot_ms,
            "metadata_ms": stats.metadata_ms,
            "apply_ms": stats.apply_ms,
        }),
    );
}

#[derive(Default)]
pub(crate) struct DiffStats {
    pub(crate) visited: usize,
    pub(crate) reconciled: usize,
    pub(crate) missing: usize,
    pub(crate) junk: usize,
    pub(crate) snapshot_ms: u64,
    pub(crate) metadata_ms: u64,
    pub(crate) apply_ms: u64,
}

fn diff_rescan_tree(
    shared: &Arc<RwLock<IndexData>>,
    root: PathBuf,
    junk: &JunkPatterns,
) -> DiffStats {
    let workers = thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1)
        .clamp(1, 4);
    diff_rescan_tree_with_workers(shared, root, junk, workers)
}

pub(crate) fn diff_rescan_tree_with_workers(
    shared: &Arc<RwLock<IndexData>>,
    root: PathBuf,
    junk: &JunkPatterns,
    workers: usize,
) -> DiffStats {
    struct DirectorySnapshot {
        id: DirId,
        path: PathBuf,
        stored_mtime: i64,
    }

    let mut stats = DiffStats::default();
    let snapshot_started = Instant::now();
    let snapshots = {
        let index = shared.read().expect("name index lock poisoned");
        let mut snapshots = Vec::with_capacity(index.node_len());
        let mut pending = vec![(0, root.clone())];
        while let Some((id, path)) = pending.pop() {
            stats.visited += 1;
            let Some(node) = index.node(id) else {
                continue;
            };
            if node.tier == Tier::Junk {
                stats.junk += 1;
                continue;
            }
            snapshots.push(DirectorySnapshot {
                id,
                path: path.clone(),
                stored_mtime: node.mtime_ms,
            });
            for (name, child_id) in index.child_dirs(id) {
                pending.push((child_id, path.join(name)));
            }
        }
        snapshots
    };
    stats.snapshot_ms = u64::try_from(snapshot_started.elapsed().as_millis()).unwrap_or(u64::MAX);

    // Metadata probes dominate a warm startup. The index snapshot is immutable until the
    // watcher gate opens, so four bounded workers can stat disjoint slices without locks.
    let mut disk_mtimes = vec![None; snapshots.len()];
    let workers = workers.clamp(1, snapshots.len().max(1));
    let chunk_size = snapshots.len().div_ceil(workers).max(1);
    let metadata_started = Instant::now();
    thread::scope(|scope| {
        for (snapshot_chunk, output_chunk) in snapshots
            .chunks(chunk_size)
            .zip(disk_mtimes.chunks_mut(chunk_size))
        {
            scope.spawn(move || {
                set_crawl_qos();
                for (snapshot, output) in snapshot_chunk.iter().zip(output_chunk) {
                    *output = fs::metadata(&snapshot.path)
                        .map(|metadata| mtime_ms(&metadata))
                        .ok();
                }
            });
        }
    });
    stats.metadata_ms = u64::try_from(metadata_started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let mut changed = Vec::new();
    for (snapshot, disk_mtime) in snapshots.into_iter().zip(disk_mtimes) {
        match disk_mtime {
            Some(disk_mtime) if disk_mtime != snapshot.stored_mtime => changed.push(snapshot),
            Some(_) => {}
            None => stats.missing += 1,
        }
    }
    changed.sort_unstable_by(|left, right| {
        left.path
            .components()
            .count()
            .cmp(&right.path.components().count())
            .then_with(|| left.path.cmp(&right.path))
    });
    let apply_started = Instant::now();
    for snapshot in changed {
        let still_same_directory = {
            let index = shared.read().expect("name index lock poisoned");
            index.node(snapshot.id).is_some() && index.full_path(snapshot.id) == snapshot.path
        };
        if still_same_directory {
            stats.reconciled += 1;
            reconcile_dir(shared, snapshot.id, &snapshot.path, &root, junk);
        }
    }
    stats.apply_ms = u64::try_from(apply_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    stats
}

/// One completed lazy Junk refresh, used by energy telemetry.
pub struct JunkDrainOutcome {
    pub dirty_dirs: usize,
    pub retained_paths: usize,
    pub exact_paths: usize,
    pub fallback_dirs: usize,
    pub duration_ms: u64,
}

struct PendingJunkDrain {
    dirty_dirs: Vec<(DirId, PathBuf)>,
    changed_paths: Vec<(Option<DirId>, PathBuf)>,
    paths_saturated: bool,
}

/// Apply every retained Junk event path, then clear the dirty set. A targeting query always calls
/// this; filesystem-idle and external-power callers are gated by the energy policy. Exact files
/// avoid enumerating their siblings. Existing directory events still reconcile direct children,
/// and a genuinely new directory is crawled once from its nearest indexed parent. If the retained
/// path cap was exceeded, fall back to reconciling the dirty parent directories.
pub fn drain_junk_dirty(
    shared: &Arc<RwLock<IndexData>>,
    root: &Path,
    junk: &JunkPatterns,
) -> JunkDrainOutcome {
    let started = Instant::now();
    let PendingJunkDrain {
        dirty_dirs: dirty,
        changed_paths,
        paths_saturated,
    } = {
        let mut index = shared.write().expect("name index lock poisoned");
        let ids: HashSet<DirId> = index.junk_dirty.drain().collect();
        let dirty = ids
            .into_iter()
            .filter_map(|id| index.node(id).map(|_| (id, index.full_path(id))))
            .collect();
        let pending_paths: Vec<PathBuf> = index.junk_changed_paths.drain().collect();
        let changed_paths = pending_paths
            .into_iter()
            .map(|path| {
                let parent = path.parent().and_then(|parent| index.resolve_dir(parent));
                (parent, path)
            })
            .collect();
        let paths_saturated = std::mem::take(&mut index.junk_paths_saturated);
        PendingJunkDrain {
            dirty_dirs: dirty,
            changed_paths,
            paths_saturated,
        }
    };
    let dirty_dirs = dirty.len();
    let retained_paths = changed_paths.len();
    let mut parent_counts = HashMap::<DirId, usize>::new();
    for (parent, _) in &changed_paths {
        if let Some(parent) = parent {
            *parent_counts.entry(*parent).or_default() += 1;
        }
    }
    let dense_parents: HashSet<DirId> = parent_counts
        .iter()
        .filter_map(|(&parent, &count)| (count > MAX_EXACT_JUNK_PATHS_PER_DIR).then_some(parent))
        .collect();
    let grouped_parents: HashSet<DirId> = parent_counts.into_keys().collect();

    let (mut exact, mut fallback): (Vec<PathBuf>, Vec<(DirId, PathBuf)>) =
        if paths_saturated || changed_paths.is_empty() {
            (Vec::new(), dirty)
        } else {
            let exact = changed_paths
                .into_iter()
                .filter_map(|(parent, path)| {
                    (!parent.is_some_and(|parent| dense_parents.contains(&parent))).then_some(path)
                })
                .collect();
            let fallback = dirty
                .into_iter()
                .filter(|(id, _)| dense_parents.contains(id) || !grouped_parents.contains(id))
                .collect();
            (exact, fallback)
        };
    let exact_paths = exact.len();
    let fallback_dirs = fallback.len();

    if !exact.is_empty() {
        exact.sort_unstable_by(|left, right| {
            left.components()
                .count()
                .cmp(&right.components().count())
                .then_with(|| left.cmp(right))
        });
        let mut indexed_subtrees = HashSet::new();
        for path in exact {
            if path
                .ancestors()
                .skip(1)
                .any(|ancestor| indexed_subtrees.contains(ancestor))
            {
                continue;
            }
            let mut index = shared.write().expect("name index lock poisoned");
            let stats = apply_fs_event_with_junk_policy(&mut index, &path, junk, false);
            if stats.indexed_subtree {
                indexed_subtrees.insert(path);
            }
        }
    }

    // A parent reconcile can remove, replace, or create a child directory. Visit parents
    // first, then verify the captured identity before touching each descendant.
    fallback.sort_unstable_by(|left, right| {
        left.1
            .components()
            .count()
            .cmp(&right.1.components().count())
            .then_with(|| left.1.cmp(&right.1))
    });
    for (id, path) in fallback {
        let still_same_directory = {
            let index = shared.read().expect("name index lock poisoned");
            index.node(id).is_some() && index.full_path(id) == path
        };
        if still_same_directory {
            reconcile_dir(shared, id, &path, root, junk);
        }
    }
    JunkDrainOutcome {
        dirty_dirs,
        retained_paths,
        exact_paths,
        fallback_dirs,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}
