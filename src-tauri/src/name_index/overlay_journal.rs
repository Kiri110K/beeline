//! Crash-safe persistence for the mutable Name Index overlay.
//!
//! The mapped v4 base stays immutable. Filesystem event paths append here before the
//! corresponding in-memory update. On the next launch Beeline re-stats those paths and
//! reconstructs the overlay, then the normal bounded diff-rescan closes any crash window.
//! Records stay until a new full base snapshot subsumes them; ordinary restarts replay the
//! same bounded set and append only paths not already represented.

use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use super::{junk::JunkPatterns, model::Tier};

const SCHEMA_VERSION: u32 = 1;
const LOG_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_UNIQUE_PATHS: usize = 100_000;
const COMPACTION_PATHS: usize = 80_000;
// NUL cannot occur in a filesystem name, so a real user path can never collide with this
// durable logical marker. Replay removes it before any filesystem call.
const JUNK_DIRTY_MARKER: &str = "\0beeline-junk-dirty";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema_version: u32,
    path: String,
}

struct Inner {
    root: PathBuf,
    path: PathBuf,
    state: Mutex<State>,
}

struct State {
    file: File,
    seen: HashSet<String>,
}

#[derive(Clone)]
pub struct OverlayJournal {
    inner: Arc<Inner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordOutcome {
    /// False means the bounded journal could not retain every new unique path. Callers
    /// that need a complete filesystem checkpoint must fall back to a full diff.
    pub all_recorded: bool,
    pub appended_paths: usize,
}

impl OverlayJournal {
    pub fn load(app_data_dir: &Path, root: &Path, junk: &JunkPatterns) -> io::Result<Self> {
        fs::create_dir_all(app_data_dir)?;
        let path = app_data_dir.join("name_index").join("home.overlay.ndjson");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let (paths, canonicalized) = read_unique(&path, junk)?;
        let truncated = paths.len() > MAX_UNIQUE_PATHS;
        let retained = paths
            .into_iter()
            .take(MAX_UNIQUE_PATHS)
            .collect::<BTreeSet<_>>();
        if canonicalized || truncated {
            rewrite_records(&path, retained.iter())?;
        }
        let seen = retained.into_iter().collect();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            inner: Arc::new(Inner {
                root: root.to_path_buf(),
                path,
                state: Mutex::new(State { file, seen }),
            }),
        })
    }

    /// Append before applying the matching in-memory mutations. A crash may therefore
    /// replay one event twice, but can never lose an acknowledged watcher batch.
    pub fn record_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a Path>,
        junk: &JunkPatterns,
    ) -> io::Result<RecordOutcome> {
        let mut relative = BTreeSet::new();
        for path in paths {
            let Ok(path) = path.strip_prefix(&self.inner.root) else {
                continue;
            };
            if valid_relative(path) {
                relative.insert(
                    canonical_relative(path, junk)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        if relative.is_empty() {
            return Ok(RecordOutcome {
                all_recorded: true,
                appended_paths: 0,
            });
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::other("overlay journal lock is poisoned"))?;
        let remaining = MAX_UNIQUE_PATHS.saturating_sub(state.seen.len());
        let mut new_paths = relative
            .into_iter()
            .filter(|path| !state.seen.contains(path))
            .collect::<Vec<_>>();
        let all_recorded = new_paths.len() <= remaining;
        new_paths.truncate(remaining);
        if new_paths.is_empty() {
            return Ok(RecordOutcome {
                all_recorded,
                appended_paths: 0,
            });
        }
        for path in &new_paths {
            serde_json::to_writer(
                &mut state.file,
                &Record {
                    schema_version: SCHEMA_VERSION,
                    path: path.clone(),
                },
            )
            .map_err(io::Error::other)?;
            state.file.write_all(b"\n")?;
        }
        state.file.flush()?;
        let appended_paths = new_paths.len();
        state.seen.extend(new_paths);
        if state.file.metadata()?.len() > LOG_BUDGET_BYTES {
            self.compact_locked(&mut state)?;
        }
        Ok(RecordOutcome {
            all_recorded,
            appended_paths,
        })
    }

    pub fn replay_paths(&self) -> io::Result<Vec<PathBuf>> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::other("overlay journal lock is poisoned"))?;
        let paths = state.seen.iter().cloned().collect::<BTreeSet<_>>();
        Ok(paths
            .into_iter()
            .map(|relative| self.inner.root.join(relative))
            .collect())
    }

    pub fn retained_paths(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.seen.len())
            .unwrap_or(MAX_UNIQUE_PATHS)
    }

    pub fn needs_base_compaction(&self) -> bool {
        self.retained_paths() >= COMPACTION_PATHS
    }

    /// Make every appended path durable before a newer FSEvents cursor is committed.
    /// Ordinary live watcher batches only need `flush`: their event IDs are newer than the
    /// saved cursor and CoreServices can replay them after a process or machine crash.
    pub fn sync(&self) -> io::Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::other("overlay journal lock is poisoned"))?;
        state.file.flush()?;
        state.file.sync_data()
    }

    /// A newly written full base already contains every earlier delta. Ordinary mapped-base
    /// startups must retain the journal across sessions.
    pub fn reset(&self) -> io::Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::other("overlay journal lock is poisoned"))?;
        state.file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.inner.path)?;
        state.seen.clear();
        Ok(())
    }

    fn compact_locked(&self, state: &mut State) -> io::Result<()> {
        state.file.flush()?;
        let paths = state.seen.iter().cloned().collect::<BTreeSet<_>>();
        let temporary = self.inner.path.with_extension("ndjson.tmp");
        let mut output = File::create(&temporary)?;
        for path in paths {
            serde_json::to_writer(
                &mut output,
                &Record {
                    schema_version: SCHEMA_VERSION,
                    path,
                },
            )
            .map_err(io::Error::other)?;
            output.write_all(b"\n")?;
        }
        output.sync_all()?;
        fs::rename(&temporary, &self.inner.path)?;
        state.file = OpenOptions::new().append(true).open(&self.inner.path)?;
        Ok(())
    }
}

fn read_unique(path: &Path, junk: &JunkPatterns) -> io::Result<(BTreeSet<String>, bool)> {
    let mut paths = BTreeSet::new();
    let mut canonicalized = false;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((paths, canonicalized));
        }
        Err(error) => return Err(error),
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Record>(&line) else {
            continue;
        };
        let relative = Path::new(&record.path);
        if record.schema_version == SCHEMA_VERSION && valid_relative(relative) {
            let canonical = canonical_relative(relative, junk)
                .to_string_lossy()
                .into_owned();
            let changed = canonical != record.path;
            let inserted = paths.insert(canonical);
            canonicalized |= changed || !inserted;
        }
    }
    Ok((paths, canonicalized))
}

fn canonical_relative(path: &Path, junk: &JunkPatterns) -> PathBuf {
    if junk_dirty_directory(path).is_some() {
        return path.to_path_buf();
    }
    if junk.classify(path) != Tier::Junk {
        return path.to_path_buf();
    }
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    if junk.classify(parent) == Tier::Junk {
        parent.join(JUNK_DIRTY_MARKER)
    } else {
        path.to_path_buf()
    }
}

pub(crate) fn junk_dirty_directory(path: &Path) -> Option<&Path> {
    (path.file_name()?.to_str()? == JUNK_DIRTY_MARKER)
        .then(|| path.parent())
        .flatten()
}

fn rewrite_records<'a>(path: &Path, paths: impl IntoIterator<Item = &'a String>) -> io::Result<()> {
    let temporary = path.with_extension("ndjson.tmp");
    let mut output = File::create(&temporary)?;
    for path in paths {
        serde_json::to_writer(
            &mut output,
            &Record {
                schema_version: SCHEMA_VERSION,
                path: path.clone(),
            },
        )
        .map_err(io::Error::other)?;
        output.write_all(b"\n")?;
    }
    output.sync_all()?;
    fs::rename(temporary, path)
}

fn valid_relative(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_index::{crawl, model::IndexData, persist, watcher};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, RwLock};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "beeline_overlay_journal_{}_{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn persists_unique_relative_paths_and_only_explicitly_resets() {
        let dir = temp_dir();
        let root = dir.join("home");
        fs::create_dir(&root).expect("root");
        let junk = JunkPatterns::default();
        let journal = OverlayJournal::load(&dir, &root, &junk).expect("load");
        journal
            .record_paths(
                [
                    root.join("work/a.txt").as_path(),
                    root.join("work/a.txt").as_path(),
                    root.join("work/b.txt").as_path(),
                    Path::new("/outside/not-recorded"),
                ],
                &junk,
            )
            .expect("record");
        journal
            .record_paths([root.join("work/a.txt").as_path()], &junk)
            .expect("record duplicate batch");

        assert_eq!(
            journal.replay_paths().expect("replay"),
            vec![root.join("work/a.txt"), root.join("work/b.txt")]
        );
        assert_eq!(
            BufReader::new(File::open(&journal.inner.path).expect("open journal"))
                .lines()
                .count(),
            2
        );
        drop(journal);
        let reloaded = OverlayJournal::load(&dir, &root, &junk).expect("reload");
        assert_eq!(reloaded.retained_paths(), 2);
        assert_eq!(
            reloaded.replay_paths().expect("replay after restart"),
            vec![root.join("work/a.txt"), root.join("work/b.txt")]
        );
        reloaded.reset().expect("explicit base checkpoint reset");
        assert!(reloaded.replay_paths().expect("empty").is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ignores_partial_obsolete_and_escaping_records() {
        let dir = temp_dir();
        let root = dir.join("home");
        fs::create_dir(&root).expect("root");
        let path = dir.join("name_index/home.overlay.ndjson");
        fs::create_dir_all(path.parent().unwrap()).expect("parent");
        fs::write(
            &path,
            "{\"schemaVersion\":1,\"path\":\"ok/file\"}\n{broken\n{\"schemaVersion\":0,\"path\":\"old\"}\n{\"schemaVersion\":1,\"path\":\"../escape\"}\n",
        )
        .expect("seed");
        let journal = OverlayJournal::load(&dir, &root, &JunkPatterns::default()).expect("load");
        assert_eq!(
            journal.replay_paths().expect("replay"),
            vec![root.join("ok/file")]
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn coalesces_junk_children_to_one_durable_dirty_directory_marker() {
        let dir = temp_dir();
        let root = dir.join("home");
        fs::create_dir(&root).expect("root");
        let junk = JunkPatterns::default();
        let journal = OverlayJournal::load(&dir, &root, &junk).expect("load");
        journal
            .record_paths(
                [
                    root.join("work/target/debug/deps/a.rlib"),
                    root.join("work/target/debug/deps/b.rlib"),
                    root.join("work/target"),
                    root.join("work/notes.txt"),
                ]
                .iter()
                .map(PathBuf::as_path),
                &junk,
            )
            .expect("record junk burst");

        assert_eq!(
            journal.replay_paths().expect("replay"),
            vec![
                root.join("work/notes.txt"),
                root.join("work/target"),
                root.join("work/target/debug/deps").join(JUNK_DIRTY_MARKER),
            ]
        );
        assert_eq!(journal.retained_paths(), 3);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_atomically_compacts_existing_junk_paths() {
        let dir = temp_dir();
        let root = dir.join("home");
        fs::create_dir(&root).expect("root");
        let path = dir.join("name_index/home.overlay.ndjson");
        fs::create_dir_all(path.parent().unwrap()).expect("parent");
        fs::write(
            &path,
            "{\"schemaVersion\":1,\"path\":\"work/target/debug/a\"}\n{\"schemaVersion\":1,\"path\":\"work/target/debug/b\"}\n",
        )
        .expect("seed uncoalesced journal");

        let junk = JunkPatterns::default();
        let journal = OverlayJournal::load(&dir, &root, &junk).expect("load");
        assert_eq!(
            journal.replay_paths().expect("replay"),
            vec![root.join("work/target/debug").join(JUNK_DIRTY_MARKER)]
        );
        assert_eq!(
            BufReader::new(File::open(&path).expect("open compacted journal"))
                .lines()
                .count(),
            1
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn durable_junk_marker_marks_the_same_directory_as_the_raw_event() {
        let dir = temp_dir();
        let root = dir.join("home");
        let parent = root.join("work/target/debug");
        fs::create_dir_all(&parent).expect("junk parent");
        let junk = JunkPatterns::default();
        let mut raw = IndexData::new(root.clone());
        let work = raw.add_dir(0, "work", Tier::Normal, 0);
        let target = raw.add_dir(work, "target", Tier::Junk, 0);
        let debug = raw.add_dir(target, "debug", Tier::Junk, 0);
        let mut marker_index = IndexData::new(root.clone());
        let work = marker_index.add_dir(0, "work", Tier::Normal, 0);
        let target = marker_index.add_dir(work, "target", Tier::Junk, 0);
        marker_index.add_dir(target, "debug", Tier::Junk, 0);
        let marker_index = Arc::new(RwLock::new(marker_index));

        crawl::apply_fs_event(&mut raw, &parent.join("artifact.o"), &junk);
        watcher::replay_paths(
            &marker_index,
            &root,
            &JunkPatterns::from_names(Vec::<String>::new()),
            vec![parent.join(JUNK_DIRTY_MARKER)],
        );

        assert_eq!(raw.junk_dirty, marker_index.read().unwrap().junk_dirty);
        assert_eq!(raw.junk_dirty, HashSet::from([debug]));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore]
    fn live_journal_coalescing_timing() {
        let source = std::env::var_os("BEELINE_LIVE_OVERLAY")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_OVERLAY");
        let index_path = std::env::var_os("BEELINE_LIVE_INDEX")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_INDEX");
        let root = std::env::var_os("BEELINE_LIVE_ROOT")
            .map(PathBuf::from)
            .expect("set BEELINE_LIVE_ROOT");
        let dir = temp_dir();
        let target = dir.join("name_index/home.overlay.ndjson");
        fs::create_dir_all(target.parent().expect("journal parent")).expect("journal parent");
        fs::copy(&source, &target).expect("copy live journal");
        let raw_replay = BufReader::new(File::open(&target).expect("open source copy"))
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Record>(&line).ok())
            .filter(|record| record.schema_version == SCHEMA_VERSION)
            .map(|record| root.join(record.path))
            .collect::<Vec<_>>();
        let before_bytes = fs::metadata(&target).expect("source metadata").len();
        let before_records = BufReader::new(File::open(&target).expect("open source copy"))
            .lines()
            .count();
        let started = std::time::Instant::now();
        let journal = OverlayJournal::load(&dir, &root, &JunkPatterns::default()).expect("load");
        let load_ms = started.elapsed().as_millis();
        let replay_started = std::time::Instant::now();
        let replay = journal.replay_paths().expect("replay");
        let replay_ms = replay_started.elapsed().as_millis();
        let raw_index = persist::load(&index_path, &root).expect("load live index");
        let raw_shared = Arc::new(RwLock::new(raw_index));
        let raw_apply_started = std::time::Instant::now();
        watcher::replay_paths(&raw_shared, &root, &JunkPatterns::default(), raw_replay);
        let raw_apply_ms = raw_apply_started.elapsed().as_millis();
        let index = persist::load(&index_path, &root).expect("reload live index");
        let shared = Arc::new(RwLock::new(index));
        let apply_started = std::time::Instant::now();
        let (applied, added, removed) =
            watcher::replay_paths(&shared, &root, &JunkPatterns::default(), replay.clone());
        let apply_ms = apply_started.elapsed().as_millis();
        let raw_index = raw_shared.read().expect("raw index lock");
        let index = shared.read().expect("index lock");
        let dirty_dirs = index.junk_dirty.len();
        assert_eq!(raw_index.len(), index.len());
        assert_eq!(raw_index.junk_dirty, index.junk_dirty);
        let after_bytes = fs::metadata(&target).expect("compacted metadata").len();
        println!(
            "before_records={before_records} before_bytes={before_bytes} retained={} after_bytes={after_bytes} load_ms={load_ms} replay_ms={replay_ms} raw_apply_ms={raw_apply_ms} apply_ms={apply_ms} applied={applied} added={added} removed={removed} dirty_dirs={dirty_dirs}",
            replay.len(),
        );
        let _ = fs::remove_dir_all(dir);
    }
}
