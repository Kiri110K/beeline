//! Crash-safe persistence for the mutable Name Index overlay.
//!
//! The mapped v4 base stays immutable. Filesystem event paths append here before the
//! corresponding in-memory update. On the next launch Beeline re-stats those paths and
//! reconstructs the overlay, then the normal bounded diff-rescan closes any crash window.
//! A successful reconciliation resets the journal before queued watcher events are released.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;
const LOG_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema_version: u32,
    path: String,
}

struct Inner {
    root: PathBuf,
    path: PathBuf,
    file: Mutex<File>,
}

#[derive(Clone)]
pub struct OverlayJournal {
    inner: Arc<Inner>,
}

impl OverlayJournal {
    pub fn load(app_data_dir: &Path, root: &Path) -> io::Result<Self> {
        fs::create_dir_all(app_data_dir)?;
        let path = app_data_dir.join("name_index").join("home.overlay.ndjson");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            inner: Arc::new(Inner {
                root: root.to_path_buf(),
                path,
                file: Mutex::new(file),
            }),
        })
    }

    /// Append before applying the matching in-memory mutations. A crash may therefore
    /// replay one event twice, but can never lose an acknowledged watcher batch.
    pub fn record_paths<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) -> io::Result<()> {
        let mut relative = Vec::new();
        for path in paths {
            let Ok(path) = path.strip_prefix(&self.inner.root) else {
                continue;
            };
            if valid_relative(path) {
                relative.push(path.to_string_lossy().into_owned());
            }
        }
        if relative.is_empty() {
            return Ok(());
        }
        let mut file = self
            .inner
            .file
            .lock()
            .map_err(|_| io::Error::other("overlay journal lock is poisoned"))?;
        for path in relative {
            serde_json::to_writer(
                &mut *file,
                &Record {
                    schema_version: SCHEMA_VERSION,
                    path,
                },
            )
            .map_err(io::Error::other)?;
            file.write_all(b"\n")?;
        }
        file.flush()?;
        if file.metadata()?.len() > LOG_BUDGET_BYTES {
            self.compact_locked(&mut file)?;
        }
        Ok(())
    }

    pub fn replay_paths(&self) -> io::Result<Vec<PathBuf>> {
        let paths = read_unique(&self.inner.path)?;
        Ok(paths
            .into_iter()
            .map(|relative| self.inner.root.join(relative))
            .collect())
    }

    /// Called only after startup replay and diff-rescan complete, while the watcher is still
    /// queueing new events. Those events append after the watcher start gate opens.
    pub fn reset(&self) -> io::Result<()> {
        let mut file = self
            .inner
            .file
            .lock()
            .map_err(|_| io::Error::other("overlay journal lock is poisoned"))?;
        *file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.inner.path)?;
        Ok(())
    }

    fn compact_locked(&self, file: &mut File) -> io::Result<()> {
        file.flush()?;
        let paths = read_unique(&self.inner.path)?;
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
        *file = OpenOptions::new().append(true).open(&self.inner.path)?;
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
    fn persists_unique_relative_paths_and_resets() {
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

        assert_eq!(
            journal.replay_paths().expect("replay"),
            vec![root.join("work/a.txt"), root.join("work/b.txt")]
        );
        journal.reset().expect("reset");
        assert!(journal.replay_paths().expect("empty").is_empty());
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
