use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const FILE_NAME: &str = "pinned_tabs.json";

// One persisted Pinned Tab: its Anchor, its optional custom name, and (through
// array order) its position. Nothing else about a Tab survives restart (§11).
// Field names cross the IPC boundary as camelCase to match the zod schema.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedPinnedTab {
    anchor_path: String,
    custom_name: Option<String>,
}

fn pinned_tabs_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join(FILE_NAME))
        .map_err(|error| format!("failed to resolve app data dir: {error}"))
}

#[tauri::command]
pub fn load_pinned_tabs(app: AppHandle) -> Result<Vec<PersistedPinnedTab>, String> {
    let path = pinned_tabs_path(&app)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // No file yet is the normal first-run case, not an error.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("failed to read pinned tabs: {error}")),
    };
    match serde_json::from_str(&text) {
        Ok(tabs) => Ok(tabs),
        // A corrupt file must not brick startup; drop it and carry on.
        Err(error) => {
            eprintln!("pinned tabs file is corrupt, ignoring: {error}");
            Ok(Vec::new())
        }
    }
}

#[tauri::command]
pub fn save_pinned_tabs(app: AppHandle, tabs: Vec<PersistedPinnedTab>) -> Result<(), String> {
    let path = pinned_tabs_path(&app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create app data dir: {error}"))?;
    }
    let json = serde_json::to_string_pretty(&tabs)
        .map_err(|error| format!("failed to encode pinned tabs: {error}"))?;
    // Atomic write: a sibling temp file, flushed, then renamed over the target so
    // a crash mid-write can never leave a half-written file.
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, json.as_bytes())
        .map_err(|error| format!("failed to write pinned tabs: {error}"))?;
    fs::rename(&temp_path, &path)
        .map_err(|error| format!("failed to replace pinned tabs: {error}"))
}
