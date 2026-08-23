//! Persisted application settings (SPEC §12). Chunk B stores only the two auto-seeded
//! application slots — the Terminal and Editor bundle ids resolved on first use (SPEC §8).
//! The full Settings UI is a later ticket (#30); this is the storage seam and nothing
//! more, following the same atomic-write pattern as [`crate::pinned_tabs`].

use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const FILE_NAME: &str = "settings.json";

/// The v1 persisted settings surface. Only the two slots exist so far; `#[serde(default)]`
/// keeps an older or partial file readable as fields are added. Field names cross the IPC
/// boundary as camelCase to match the zod schema.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// Bundle id of the resolved Terminal slot, or `None` until first seeded (SPEC §8).
    terminal_bundle_id: Option<String>,
    /// Bundle id of the resolved Editor slot, or `None` until first seeded (SPEC §8).
    editor_bundle_id: Option<String>,
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join(FILE_NAME))
        .map_err(|error| format!("failed to resolve app data dir: {error}"))
}

#[tauri::command]
pub fn load_app_settings(app: AppHandle) -> Result<AppSettings, String> {
    let path = settings_path(&app)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // No file yet is the normal first-run case, not an error.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(AppSettings::default()),
        Err(error) => return Err(format!("failed to read settings: {error}")),
    };
    match serde_json::from_str(&text) {
        Ok(settings) => Ok(settings),
        // A corrupt file must not brick startup; fall back to defaults and carry on.
        Err(error) => {
            eprintln!("settings file is corrupt, ignoring: {error}");
            Ok(AppSettings::default())
        }
    }
}

#[tauri::command]
pub fn save_app_settings(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    let path = settings_path(&app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create app data dir: {error}"))?;
    }
    let json = serde_json::to_string_pretty(&settings)
        .map_err(|error| format!("failed to encode settings: {error}"))?;
    // Atomic write: a sibling temp file, then a rename over the target, so a crash
    // mid-write can never leave a half-written file (matches pinned_tabs.rs).
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, json.as_bytes())
        .map_err(|error| format!("failed to write settings: {error}"))?;
    fs::rename(&temp_path, &path).map_err(|error| format!("failed to replace settings: {error}"))
}
