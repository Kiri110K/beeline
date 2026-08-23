use std::{
    fs, io,
    path::Path,
    time::{Instant, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::json;
use tauri::State;

use crate::telemetry::Telemetry;

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

// A tagged, code-only error: raw io::Error text never reaches the UI.
#[derive(Serialize)]
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

fn kind_label(name: &str, is_directory: bool) -> String {
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

fn read_location(path: &str) -> Result<ListLocation, ListError> {
    let metadata = fs::metadata(path).map_err(|error| map_io_error(&error))?;
    if !metadata.is_dir() {
        return Err(ListError::NotADirectory);
    }

    let mut items = Vec::new();
    for entry in fs::read_dir(path).map_err(|error| map_io_error(&error))? {
        // Skip entries that error mid-scan rather than failing the whole listing.
        let Ok(entry) = entry else { continue };
        items.push(describe_entry(&entry));
    }

    // Directories and files intermixed, case-insensitive by name.
    items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(ListLocation {
        path: path.to_owned(),
        items,
    })
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
                "duration_ms": duration_ms,
            }),
        ) {
            eprintln!("telemetry event location_listed failed: {error}");
        }
    }

    listing
}
