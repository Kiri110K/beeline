use std::{
    fs, io,
    path::Path,
    time::{Instant, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::json;
use tauri::State;

use crate::telemetry::Telemetry;

// A large directory paints from this sorted prefix, then the frontend asks for the full
// metadata-rich listing in the background. This is comfortably above one viewport while
// keeping the first-screen path free of tens of thousands of stat calls (SPEC §10).
const INITIAL_LIST_ITEMS: usize = 64;

// One directory entry, shaped for the dense listing. Field names cross the IPC
// boundary as camelCase to match the zod schema on the TypeScript side.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListedItem {
    name: String,
    path: String,
    is_directory: bool,
    is_hidden: bool,
    kind: String,
    modified_ms: Option<i64>,
    size_bytes: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListLocation {
    path: String,
    items: Vec<ListedItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialListLocation {
    path: String,
    items: Vec<ListedItem>,
    total: usize,
    complete: bool,
}

struct SortableEntry {
    entry: fs::DirEntry,
    sort_key: String,
}

// A tagged, code-only error: raw io::Error text never reaches the UI.
#[derive(Debug, Serialize)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum ListError {
    NotFound,
    NotADirectory,
    PermissionDenied,
    Io,
}

fn map_io_error(error: &io::Error) -> ListError {
    match error.kind() {
        io::ErrorKind::NotFound => ListError::NotFound,
        io::ErrorKind::PermissionDenied => ListError::PermissionDenied,
        _ => ListError::Io,
    }
}

pub(crate) fn kind_label(name: &str, is_directory: bool) -> String {
    if is_directory {
        return "Folder".to_owned();
    }
    match Path::new(name).extension().and_then(|ext| ext.to_str()) {
        Some(ext) if !ext.is_empty() => ext.to_uppercase(),
        _ => "File".to_owned(),
    }
}

fn modified_ms(metadata: &fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

fn describe_entry(entry: &fs::DirEntry) -> ListedItem {
    let name = entry.file_name().to_string_lossy().into_owned();
    let entry_path = entry.path();
    // metadata() follows symlinks so a symlinked directory is navigable.
    let metadata = fs::metadata(&entry_path).ok();
    let is_directory = metadata.as_ref().is_some_and(fs::Metadata::is_dir);
    let is_hidden = name.starts_with('.');
    let kind = kind_label(&name, is_directory);
    let modified_ms = metadata.as_ref().and_then(modified_ms);
    let size_bytes = match &metadata {
        Some(meta) if !is_directory => Some(meta.len()),
        _ => None,
    };

    ListedItem {
        name,
        path: entry_path.to_string_lossy().into_owned(),
        is_directory,
        is_hidden,
        kind,
        modified_ms,
        size_bytes,
    }
}

fn sorted_entries(path: &str) -> Result<Vec<SortableEntry>, ListError> {
    let metadata = fs::metadata(path).map_err(|error| map_io_error(&error))?;
    if !metadata.is_dir() {
        return Err(ListError::NotADirectory);
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(path).map_err(|error| map_io_error(&error))? {
        // Skip entries that error mid-scan rather than failing the whole listing.
        let Ok(entry) = entry else { continue };
        let sort_key = entry.file_name().to_string_lossy().to_lowercase();
        entries.push(SortableEntry { entry, sort_key });
    }

    // Directories and files intermixed, case-insensitive by name.
    entries.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    Ok(entries)
}

fn read_location(path: &str) -> Result<ListLocation, ListError> {
    let items = sorted_entries(path)?
        .into_iter()
        .map(|entry| describe_entry(&entry.entry))
        .collect();

    Ok(ListLocation {
        path: path.to_owned(),
        items,
    })
}

fn read_initial_location(path: &str) -> Result<InitialListLocation, ListError> {
    let entries = sorted_entries(path)?;
    let total = entries.len();
    let complete = total <= INITIAL_LIST_ITEMS;
    let items = entries
        .into_iter()
        .take(INITIAL_LIST_ITEMS)
        .map(|entry| describe_entry(&entry.entry))
        .collect();

    Ok(InitialListLocation {
        path: path.to_owned(),
        items,
        total,
        complete,
    })
}

#[tauri::command]
pub async fn list_location_initial(
    path: String,
    telemetry: State<'_, Telemetry>,
) -> Result<InitialListLocation, ListError> {
    let started = Instant::now();
    let task_path = path.clone();
    let listing = tauri::async_runtime::spawn_blocking(move || read_initial_location(&task_path))
        .await
        .map_err(|_| ListError::Io)?;

    if let Ok(location) = &listing {
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Err(error) = telemetry.record(
            "location_listed",
            json!({
                "path": path,
                "count": location.total,
                "returned": location.items.len(),
                "complete": location.complete,
                "phase": "initial",
                "duration_ms": duration_ms,
            }),
        ) {
            eprintln!("telemetry event location_listed failed: {error}");
        }
    }

    listing
}

#[tauri::command]
pub async fn list_location(
    path: String,
    telemetry: State<'_, Telemetry>,
) -> Result<ListLocation, ListError> {
    let started = Instant::now();
    let task_path = path.clone();
    let listing = tauri::async_runtime::spawn_blocking(move || read_location(&task_path))
        .await
        .map_err(|_| ListError::Io)?;

    if let Ok(location) = &listing {
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Err(error) = telemetry.record(
            "location_listed",
            json!({
                "path": path,
                "count": location.items.len(),
                "returned": location.items.len(),
                "complete": true,
                "phase": "full",
                "duration_ms": duration_ms,
            }),
        ) {
            eprintln!("telemetry event location_listed failed: {error}");
        }
    }

    listing
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("beeline-listing-test-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create listing test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn large_initial_listing_is_sorted_and_bounded() {
        let dir = TestDir::new();
        for index in (0..INITIAL_LIST_ITEMS + 2).rev() {
            fs::write(dir.0.join(format!("file_{index:04}.txt")), b"x")
                .expect("write listing fixture");
        }

        let initial = read_initial_location(dir.0.to_str().expect("utf-8 test path"))
            .expect("read initial listing");

        assert_eq!(initial.total, INITIAL_LIST_ITEMS + 2);
        assert_eq!(initial.items.len(), INITIAL_LIST_ITEMS);
        assert!(!initial.complete);
        assert_eq!(
            initial.items.first().map(|item| item.name.as_str()),
            Some("file_0000.txt")
        );
        let expected_last = format!("file_{:04}.txt", INITIAL_LIST_ITEMS - 1);
        assert_eq!(
            initial.items.last().map(|item| item.name.as_str()),
            Some(expected_last.as_str())
        );
    }
}
