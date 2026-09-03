//! The Visit Journal (SPEC §6, §11, glossary): the application's local, never-user-facing
//! record of Locations entered and files opened through the app. Its only job is to feed
//! Search ranking — recently and frequently visited paths rank higher.
//!
//! On disk it is an append-only NDJSON log (`visit_journal.ndjson`) in the app data dir.
//! Every `record` appends one line and updates the in-memory aggregate; there is no
//! periodic flush (SPEC §11: append-on-record). On start the log is replayed into the
//! aggregate. Recording is liberal in v1 and pruning the log is out of scope (SPEC §6).
//!
//! Determinism: the ranking boost is a pure function of the aggregate, with recency
//! measured against the newest visit in the journal (not against the wall clock at query
//! time), so the same journal always yields the same order.

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Mutex, RwLock, RwLockReadGuard},
};

use serde::{Deserialize, Serialize};

/// Frequency weight per visit, capped at [`FREQ_CAP_VISITS`] visits.
const FREQ_WEIGHT: i64 = 2_000;
/// Visits beyond this count no longer raise the frequency component.
const FREQ_CAP_VISITS: i64 = 10;
/// Maximum recency component (awarded to a path visited at the journal's newest moment).
const RECENCY_MAX: i64 = 10_000;
/// Upper bound on the whole visit boost, so it can never cross a match-quality band.
pub const VISIT_CAP: i64 = 30_000;
/// One day in milliseconds, the unit of the recency decay.
const DAY_MS: i64 = 86_400_000;

/// What a visit was: entering a Location, or opening a file (SPEC §6). Both feed ranking
/// identically today; the distinction is kept for later tuning and analysis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisitKind {
    EnteredLocation,
    OpenedFile,
}

impl VisitKind {
    /// Parse the wire string used by the `record_visit` command.
    pub fn parse(value: &str) -> Option<VisitKind> {
        match value {
            "entered_location" => Some(VisitKind::EnteredLocation),
            "opened_file" => Some(VisitKind::OpenedFile),
            _ => None,
        }
    }
}

/// One journal line as written to disk.
#[derive(Serialize, Deserialize)]
struct VisitRecord {
    path: String,
    kind: VisitKind,
    timestamp_ms: i64,
}

/// Per-path visit statistics.
#[derive(Clone, Copy)]
struct VisitStats {
    count: u32,
    last_ms: i64,
}

/// The in-memory rollup the ranker reads. Cheap to build (one pass over the log) and
/// cheap to query (one hash lookup per candidate).
#[derive(Default)]
pub struct Aggregate {
    by_path: HashMap<String, VisitStats>,
    /// The newest `last_ms` across all paths; the reference point for recency decay.
    newest_ms: i64,
}

impl Aggregate {
    fn apply(&mut self, path: &str, timestamp_ms: i64) {
        let stats = self.by_path.entry(path.to_owned()).or_insert(VisitStats {
            count: 0,
            last_ms: timestamp_ms,
        });
        stats.count = stats.count.saturating_add(1);
        stats.last_ms = stats.last_ms.max(timestamp_ms);
        self.newest_ms = self.newest_ms.max(timestamp_ms);
    }

    /// The ranking boost for `path`: recency-weighted frequency, `0` if never visited.
    /// Deterministic — recency is measured from the journal's newest visit, so it does
    /// not depend on when the query runs.
    pub fn boost(&self, path: &str) -> i64 {
        let Some(stats) = self.by_path.get(path) else {
            return 0;
        };
        let freq = i64::from(stats.count).min(FREQ_CAP_VISITS) * FREQ_WEIGHT;
        let age_days = (self.newest_ms - stats.last_ms).max(0) / DAY_MS;
        let recency = RECENCY_MAX / (1 + age_days);
        (freq + recency).min(VISIT_CAP)
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.by_path.keys().map(String::as_str)
    }
}

/// The live Visit Journal: an append handle plus the in-memory aggregate.
pub struct VisitJournal {
    file: Mutex<File>,
    aggregate: RwLock<Aggregate>,
}

impl VisitJournal {
    /// Path of the journal file in the app data dir.
    pub fn journal_path(app_data_dir: &Path) -> PathBuf {
        app_data_dir.join("visit_journal.ndjson")
    }

    /// Load the journal, replaying any existing log into the aggregate and opening the
    /// file for appends. A missing or partly corrupt log degrades to whatever lines
    /// parsed — it never fails init.
    pub fn load(app_data_dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(app_data_dir)?;
        let path = Self::journal_path(app_data_dir);

        let mut aggregate = Aggregate::default();
        if let Ok(existing) = File::open(&path) {
            for line in BufReader::new(existing).lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(record) = serde_json::from_str::<VisitRecord>(&line) {
                    aggregate.apply(&record.path, record.timestamp_ms);
                }
            }
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            file: Mutex::new(file),
            aggregate: RwLock::new(aggregate),
        })
    }

    /// Record a visit: append one NDJSON line and fold it into the aggregate.
    pub fn record(&self, path: &str, kind: VisitKind, timestamp_ms: i64) -> io::Result<()> {
        let record = VisitRecord {
            path: path.to_owned(),
            kind,
            timestamp_ms,
        };
        let line = serde_json::to_string(&record).map_err(io::Error::other)?;

        {
            let mut file = self
                .file
                .lock()
                .map_err(|_| io::Error::other("visit journal file lock is poisoned"))?;
            file.write_all(line.as_bytes())?;
            file.write_all(b"\n")?;
            file.flush()?;
        }

        self.aggregate
            .write()
            .map_err(|_| io::Error::other("visit journal aggregate lock is poisoned"))?
            .apply(path, timestamp_ms);
        Ok(())
    }

    /// Borrow the aggregate for ranking.
    pub fn aggregate(&self) -> RwLockReadGuard<'_, Aggregate> {
        self.aggregate.read().expect("visit journal lock poisoned")
    }
}

#[cfg(test)]
impl Aggregate {
    /// Build an aggregate directly from `(path, timestamp_ms)` visits, for ranker tests.
    pub fn from_visits(visits: &[(&str, i64)]) -> Self {
        let mut aggregate = Aggregate::default();
        for (path, timestamp) in visits {
            aggregate.apply(path, *timestamp);
        }
        aggregate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("beeline_vj_{}_{}", std::process::id(), unique));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn frequency_and_recency_raise_the_boost() {
        // More frequent → higher; among equal frequency, more recent → higher.
        let often = Aggregate::from_visits(&[("/p", 10), ("/p", 20), ("/p", 30)]);
        let once = Aggregate::from_visits(&[("/p", 30)]);
        assert!(often.boost("/p") > once.boost("/p"));

        // With the same count, a later last-visit (relative to the journal's newest)
        // scores higher. Recency decays per day, so the gap must be day-scale.
        let recent = Aggregate::from_visits(&[("/a", 100), ("/b", 100)]);
        assert_eq!(recent.boost("/a"), recent.boost("/b"));
        let mixed = Aggregate::from_visits(&[("/a", 100), ("/b", 100 + 3 * DAY_MS)]);
        assert!(mixed.boost("/b") > mixed.boost("/a"));

        // Never-visited paths get no boost.
        assert_eq!(once.boost("/unknown"), 0);
    }

    #[test]
    fn append_reload_round_trip() {
        let dir = TempDir::new();
        {
            let journal = VisitJournal::load(&dir.0).expect("load");
            journal
                .record("/home/tester/Downloads", VisitKind::EnteredLocation, 1_000)
                .expect("record");
            journal
                .record("/home/tester/a.txt", VisitKind::OpenedFile, 2_000)
                .expect("record");
            assert!(journal.aggregate().boost("/home/tester/Downloads") > 0);
        }

        // A fresh load replays the on-disk log.
        let reloaded = VisitJournal::load(&dir.0).expect("reload");
        let aggregate = reloaded.aggregate();
        assert!(aggregate.boost("/home/tester/Downloads") > 0);
        assert!(aggregate.boost("/home/tester/a.txt") > 0);
    }
}
