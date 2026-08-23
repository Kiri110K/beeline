use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::Instant,
};

use serde_json::{json, Map, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub struct Telemetry {
    file: Mutex<File>,
    path: PathBuf,
    pid: u32,
    started: Instant,
}

impl Telemetry {
    pub fn new(app_data_dir: &Path, started: Instant) -> io::Result<Self> {
        fs::create_dir_all(app_data_dir)?;

        let path = app_data_dir.join("telemetry.ndjson");
        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        Ok(Self {
            file: Mutex::new(file),
            path,
            pid: std::process::id(),
            started,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record(&self, event: &str, fields: Value) -> io::Result<()> {
        let Value::Object(fields) = fields else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "telemetry fields must be a JSON object",
            ));
        };

        let timestamp = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(io::Error::other)?;

        let mut record = Map::new();
        record.insert("event".into(), Value::String(event.into()));
        record.insert(
            "monotonic_ns".into(),
            json!(self.started.elapsed().as_nanos()),
        );
        record.insert("timestamp".into(), Value::String(timestamp));
        record.insert("pid".into(), json!(self.pid));

        // Envelope fields (event/monotonic_ns/timestamp/pid) are authoritative;
        // caller-supplied fields only fill keys the envelope has not claimed.
        for (key, value) in fields {
            if !record.contains_key(&key) {
                record.insert(key, value);
            }
        }

        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("telemetry file lock is poisoned"))?;
        serde_json::to_writer(&mut *file, &record).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.flush()
    }
}
