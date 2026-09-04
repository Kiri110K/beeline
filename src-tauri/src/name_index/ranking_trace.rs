//! Local, replayable Ranking Traces (SPEC §6.5, §11).
//!
//! The search path only sends small owned messages. One event-driven worker owns the append
//! file and a single 300 ms detailed-trace debounce; there is no polling and no thread per
//! keystroke. Compact query records and action records append immediately. The newest final
//! result snapshot replaces an older pending detail record until the query stays idle.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::Path,
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use super::{query::SearchHit, ranker_config::RankerConfig};

const SCHEMA_VERSION: u32 = 1;
const DETAIL_IDLE: Duration = Duration::from_millis(300);
const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const MAINTENANCE_INTERVAL_MS: i64 = 24 * 60 * 60 * 1_000;
const LOG_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
const COMPACT_TARGET_BYTES: usize = 192 * 1024 * 1024;

#[derive(Clone)]
pub struct RankingTraces {
    sender: Sender<Event>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Query {
        schema_version: u32,
        timestamp_ms: i64,
        query: String,
        config_fingerprint: String,
        significant_chars: usize,
        global_requested: bool,
    },
    Results {
        schema_version: u32,
        timestamp_ms: i64,
        query: String,
        config_fingerprint: String,
        stage: String,
        backend_duration_ms: u64,
        scanned: usize,
        results: Vec<RankedItem>,
    },
    Action {
        schema_version: u32,
        timestamp_ms: i64,
        query: Option<String>,
        path: String,
        signal: String,
        config_fingerprint: String,
    },
}

impl Record {
    fn timestamp_ms(&self) -> i64 {
        match self {
            Self::Query { timestamp_ms, .. }
            | Self::Results { timestamp_ms, .. }
            | Self::Action { timestamp_ms, .. } => *timestamp_ms,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RankedItem {
    rank: usize,
    path: String,
    name: String,
    score: i64,
    tier: String,
    is_directory: bool,
}

enum Event {
    Append(Record),
    ScheduleDetail(Record),
    SnapshotConfig {
        fingerprint: String,
        formatted: String,
    },
}

impl RankingTraces {
    pub fn init(app_data_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(app_data_dir)?;
        let trace_path = app_data_dir.join("ranking_traces.ndjson");
        let snapshots = app_data_dir.join("ranker_snapshots");
        fs::create_dir_all(&snapshots)?;
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("beeline-ranking-traces".to_owned())
            .spawn(move || {
                crate::qos::set_qos(0x11);
                if let Err(error) = run_writer(&trace_path, &snapshots, receiver) {
                    eprintln!("ranking trace writer failed: {error}");
                }
            })?;
        Ok(Self { sender })
    }

    pub fn snapshot_config(&self, config: &RankerConfig) {
        let _ = self.sender.send(Event::SnapshotConfig {
            fingerprint: config.fingerprint(),
            formatted: serde_json::to_string_pretty(config)
                .expect("Ranker Configuration serializes"),
        });
    }

    pub fn query_started(
        &self,
        timestamp_ms: i64,
        query: &str,
        config: &RankerConfig,
        significant_chars: usize,
        global_requested: bool,
    ) {
        let _ = self.sender.send(Event::Append(Record::Query {
            schema_version: SCHEMA_VERSION,
            timestamp_ms,
            query: query.to_owned(),
            config_fingerprint: config.fingerprint(),
            significant_chars,
            global_requested,
        }));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn results_complete(
        &self,
        timestamp_ms: i64,
        query: &str,
        config: &RankerConfig,
        stage: &str,
        backend_duration_ms: u64,
        scanned: usize,
        hits: &[SearchHit],
    ) {
        let results = hits
            .iter()
            .enumerate()
            .map(|(index, hit)| RankedItem {
                rank: index + 1,
                path: hit.path.clone(),
                name: hit.name.clone(),
                score: hit.score,
                tier: hit.tier.to_owned(),
                is_directory: hit.is_directory,
            })
            .collect();
        let _ = self.sender.send(Event::ScheduleDetail(Record::Results {
            schema_version: SCHEMA_VERSION,
            timestamp_ms,
            query: query.to_owned(),
            config_fingerprint: config.fingerprint(),
            stage: stage.to_owned(),
            backend_duration_ms,
            scanned,
            results,
        }));
    }

    pub fn action(
        &self,
        timestamp_ms: i64,
        query: Option<&str>,
        path: &str,
        signal: &str,
        config: &RankerConfig,
    ) {
        let _ = self.sender.send(Event::Append(Record::Action {
            schema_version: SCHEMA_VERSION,
            timestamp_ms,
            query: query.map(str::to_owned),
            path: path.to_owned(),
            signal: signal.to_owned(),
            config_fingerprint: config.fingerprint(),
        }));
    }
}

fn run_writer(
    trace_path: &Path,
    snapshots: &Path,
    receiver: mpsc::Receiver<Event>,
) -> io::Result<()> {
    let mut last_maintenance_ms = wall_clock_ms();
    compact_if_needed(trace_path, last_maintenance_ms)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(trace_path)?;
    let mut pending: Option<(Instant, Record)> = None;
    loop {
        let event = match pending.as_ref() {
            Some((deadline, _)) => {
                let timeout = deadline.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(timeout) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => {
                        let (_, record) = pending.take().expect("pending detail exists");
                        append(&mut file, &record)?;
                        compact_after_write(
                            trace_path,
                            &mut file,
                            record.timestamp_ms(),
                            &mut last_maintenance_ms,
                        )?;
                        None
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match receiver.recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            },
        };
        let Some(event) = event else {
            continue;
        };
        match event {
            Event::Append(record) => {
                if matches!(record, Record::Query { .. }) {
                    pending = None;
                }
                append(&mut file, &record)?;
                compact_after_write(
                    trace_path,
                    &mut file,
                    record.timestamp_ms(),
                    &mut last_maintenance_ms,
                )?;
            }
            Event::ScheduleDetail(record) => {
                pending = Some((Instant::now() + DETAIL_IDLE, record));
            }
            Event::SnapshotConfig {
                fingerprint,
                formatted,
            } => snapshot_config(snapshots, &fingerprint, &formatted)?,
        }
    }
    if let Some((_, record)) = pending {
        append(&mut file, &record)?;
    }
    file.flush()
}

fn append(file: &mut File, record: &Record) -> io::Result<()> {
    serde_json::to_writer(&mut *file, record).map_err(io::Error::other)?;
    file.write_all(b"\n")
}

fn snapshot_config(directory: &Path, fingerprint: &str, formatted: &str) -> io::Result<()> {
    let target = directory.join(format!("{fingerprint}.json"));
    if target.exists() {
        return Ok(());
    }
    let temp = directory.join(format!("{fingerprint}.json.tmp"));
    let mut output = File::create(&temp)?;
    output.write_all(formatted.as_bytes())?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    fs::rename(temp, target)
}

fn compact_after_write(
    path: &Path,
    file: &mut File,
    newest_ms: i64,
    last_maintenance_ms: &mut i64,
) -> io::Result<()> {
    if file.metadata()?.len() <= LOG_BUDGET_BYTES
        && newest_ms.saturating_sub(*last_maintenance_ms) < MAINTENANCE_INTERVAL_MS
    {
        return Ok(());
    }
    file.flush()?;
    compact_if_needed(path, newest_ms)?;
    *last_maintenance_ms = newest_ms;
    *file = OpenOptions::new().append(true).open(path)?;
    Ok(())
}

fn compact_if_needed(path: &Path, newest_ms: i64) -> io::Result<()> {
    match fs::metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let cutoff = newest_ms.saturating_sub(RETENTION_MS);
    let mut retained = Vec::new();
    let mut bytes = 0usize;
    for line in BufReader::new(File::open(path)?)
        .lines()
        .map_while(Result::ok)
    {
        let timestamp = serde_json::from_str::<serde_json::Value>(&line)
            .ok()
            .and_then(|value| {
                value
                    .get("timestamp_ms")
                    .and_then(serde_json::Value::as_i64)
            })
            .unwrap_or(i64::MIN);
        if timestamp >= cutoff {
            bytes = bytes.saturating_add(line.len() + 1);
            retained.push(line);
        }
    }
    let mut drop_count = 0usize;
    while bytes > COMPACT_TARGET_BYTES && drop_count < retained.len() {
        bytes = bytes.saturating_sub(retained[drop_count].len() + 1);
        drop_count += 1;
    }
    let temp = path.with_extension("ndjson.tmp");
    let mut output = File::create(&temp)?;
    for line in &retained[drop_count..] {
        output.write_all(line.as_bytes())?;
        output.write_all(b"\n")?;
    }
    output.sync_all()?;
    fs::rename(temp, path)
}

fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "beeline_ranking_trace_{}_{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn config_snapshot_is_content_addressed() {
        let dir = temp_dir();
        let config = RankerConfig::default();
        snapshot_config(&dir, &config.fingerprint(), "first").expect("first");
        snapshot_config(&dir, &config.fingerprint(), "second").expect("second");
        let text = fs::read_to_string(dir.join(format!("{}.json", config.fingerprint())))
            .expect("snapshot");
        assert_eq!(text, "first\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn append_writes_replayable_tagged_records() {
        let dir = temp_dir();
        let path = dir.join("trace.ndjson");
        let mut file = File::create(&path).expect("create");
        append(
            &mut file,
            &Record::Query {
                schema_version: SCHEMA_VERSION,
                timestamp_ms: 10,
                query: "methodology".to_owned(),
                config_fingerprint: "abc".to_owned(),
                significant_chars: 11,
                global_requested: true,
            },
        )
        .expect("append");
        drop(file);
        let value: serde_json::Value =
            serde_json::from_str(fs::read_to_string(path).expect("read").trim()).expect("json");
        assert_eq!(value["kind"], "query");
        assert_eq!(value["query"], "methodology");
        let _ = fs::remove_dir_all(dir);
    }
}
