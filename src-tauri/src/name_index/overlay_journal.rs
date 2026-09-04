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

const SCHEMA_VERSION: u32 = 1;
const LOG_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_UNIQUE_PATHS: usize = 100_000;
const COMPACTION_PATHS: usize = 80_000;

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
    pub fn load(app_data_dir: &Path, root: &Path) -> io::Result<Self> {
        fs::create_dir_all(app_data_dir)?;
        let path = app_data_dir.join("name_index").join("home.overlay.ndjson");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let seen = read_unique(&path)?
            .into_iter()
            .take(MAX_UNIQUE_PATHS)
            .collect();
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
    ) -> io::Result<RecordOutcome> {
        let mut relative = BTreeSet::new();
        for path in paths {
            let Ok(path) = path.strip_prefix(&self.inner.root) else {
                continue;
            };
            if valid_relative(path) {
                relative.insert(path.to_string_lossy().into_owned());
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

fn read_unique(path: &Path) -> io::Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => return Err(error),
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Record>(&line) else {
            continue;
        };
        let relative = Path::new(&record.path);
        if record.schema_version == SCHEMA_VERSION && valid_relative(relative) {
            paths.insert(record.path);
        }
    }
    Ok(paths)
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
    use std::sync::atomic::{AtomicU64, Ordering};

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
        let journal = OverlayJournal::load(&dir, &root).expect("load");
        journal
            .record_paths([
                root.join("work/a.txt").as_path(),
                root.join("work/a.txt").as_path(),
                root.join("work/b.txt").as_path(),
                Path::new("/outside/not-recorded"),
            ])
            .expect("record");
        journal
            .record_paths([root.join("work/a.txt").as_path()])
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
        let reloaded = OverlayJournal::load(&dir, &root).expect("reload");
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
        let journal = OverlayJournal::load(&dir, &root).expect("load");
        assert_eq!(
            journal.replay_paths().expect("replay"),
            vec![root.join("ok/file")]
        );
        let _ = fs::remove_dir_all(dir);
    }
}
