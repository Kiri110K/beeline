//! The one persisted settings store (SPEC §12). A single versioned JSON file, written
//! atomically (a sibling temp file then a rename, matching [`crate::pinned_tabs`]), holds
//! the whole v1 Settings inventory plus the internal first-run flag (§13 — not a user
//! setting, so it never appears in the Settings UI).
//!
//! Rust owns only storage and the two consumers that live here (the Name Index's Junk
//! patterns and Alias Dictionary; the global shortcut re-registration lives in `lib.rs`
//! where the plugin handle is). Field names cross the IPC boundary as camelCase to match
//! the frontend zod schema; the whole surface is parse-don't-validate on both sides.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const FILE_NAME: &str = "settings.json";
/// The legacy Alias Dictionary file (SPEC §6), absorbed into the settings store on the
/// first launch that has no settings file yet.
const LEGACY_ALIASES_FILE: &str = "aliases.json";

/// The configurable global shortcut (SPEC §2, §12). Modelled by the browser's own
/// `KeyboardEvent.code` (e.g. `"KeyF"`) plus modifier flags, so the capture field on the
/// frontend and the `Code`/`Modifiers` on the Rust side share one representation with no
/// accelerator-string parsing in the middle.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutSpec {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    /// A W3C `KeyboardEvent.code` value (`"KeyF"`, `"Digit1"`, `"Slash"`, …).
    pub code: String,
}

/// The Default Entry Point of a new Temporary Tab (SPEC §4, §12): Recents, or a directory.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EntryPoint {
    Recents,
    Directory { path: String },
}

/// The Temporary Tab lifetime (SPEC §4, §12): a positive minute count, or never expiring.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Lifetime {
    Minutes { minutes: u32 },
    Never,
}

/// One Alias Dictionary entry (SPEC §6): a typed word mapped to a target Location.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AliasEntry {
    pub word: String,
    pub path: String,
}

/// The complete v1 persisted settings surface (SPEC §12) plus the internal first-run flag.
/// `#[serde(default)]` on the container fills any missing field from [`AppSettings::default`],
/// so an older or partial file — or a brand-new install with no file — reads cleanly.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// Global shortcut, default `Ctrl+Opt+Cmd+F` (SPEC §2).
    pub global_shortcut: ShortcutSpec,
    /// Default Entry Point, default Recents (SPEC §4).
    pub default_entry_point: EntryPoint,
    /// Temporary Tab lifetime, default 3 hours (SPEC §4).
    pub temporary_tab_lifetime: Lifetime,
    /// Directory primary action: `"enter"` or `"menu"` (SPEC §5). Stored opaquely; the
    /// frontend parses it into a typed enum.
    pub primary_action_directory: String,
    /// File primary action: `"open"` or `"menu"` (SPEC §5).
    pub primary_action_file: String,
    /// After Action table: action id → `"hide"` / `"keep"` (SPEC §12). Stored opaquely.
    pub after_action: BTreeMap<String, String>,
    /// Preview Panel visibility, default on (SPEC §9).
    pub preview_panel_visible: bool,
    /// Terminal slot bundle id, or `None` until first seeded (SPEC §8).
    pub terminal_bundle_id: Option<String>,
    /// Editor slot bundle id, or `None` until first seeded (SPEC §8).
    pub editor_bundle_id: Option<String>,
    /// Alias Dictionary entries (SPEC §6). Feeds the Name Index ranker.
    pub aliases: Vec<AliasEntry>,
    /// Junk pattern list (SPEC §6). Feeds the Name Index classifier.
    pub junk_patterns: Vec<String>,
    /// Whether the one-time first-run guidance has been dismissed (SPEC §13). Internal
    /// persistence, not a user setting — never rendered in the Settings UI.
    pub first_run_dismissed: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            global_shortcut: default_shortcut_spec(),
            default_entry_point: EntryPoint::Recents,
            temporary_tab_lifetime: Lifetime::Minutes { minutes: 180 },
            primary_action_directory: "enter".to_owned(),
            primary_action_file: "open".to_owned(),
            after_action: default_after_action(),
            preview_panel_visible: true,
            terminal_bundle_id: None,
            editor_bundle_id: None,
            aliases: Vec::new(),
            junk_patterns: crate::name_index::builtin_junk_names(),
            first_run_dismissed: false,
        }
    }
}

/// The default global shortcut, `Ctrl+Opt+Cmd+F` (SPEC §2).
fn default_shortcut_spec() -> ShortcutSpec {
    ShortcutSpec {
        control: true,
        alt: true,
        shift: false,
        meta: true,
        code: "KeyF".to_owned(),
    }
}

/// The After Action defaults (SPEC §12): hide after Open File, Open in Terminal/Editor,
/// Copy Path, Copy File; keep the window open after everything else. Keyed by the frontend's
/// `AfterActionId` strings so both sides agree on the row set.
fn default_after_action() -> BTreeMap<String, String> {
    const HIDE: [&str; 5] = [
        "open_file",
        "open_terminal",
        "open_editor",
        "copy_path",
        "copy_file",
    ];
    const KEEP: [&str; 10] = [
        "trash",
        "delete_permanently",
        "enter_directory",
        "navigation",
        "paste",
        "move_paste",
        "rename",
        "new_folder",
        "reveal",
        "open_in_new_tab",
    ];
    let mut table = BTreeMap::new();
    for id in HIDE {
        table.insert(id.to_owned(), "hide".to_owned());
    }
    for id in KEEP {
        table.insert(id.to_owned(), "keep".to_owned());
    }
    table
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join(FILE_NAME))
        .map_err(|error| format!("failed to resolve app data dir: {error}"))
}

/// Read the settings from `app_data_dir`, returning both the settings and whether the file
/// existed. A missing file yields the defaults (SPEC §13 first-run trigger); a corrupt file
/// must never brick startup, so it also falls back to defaults. On a first, fileless launch
/// the legacy `aliases.json` is absorbed so a pre-Settings Alias Dictionary survives.
pub fn read(app_data_dir: &Path) -> (AppSettings, bool) {
    let path = app_data_dir.join(FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let defaults = AppSettings {
                aliases: migrate_legacy_aliases(app_data_dir),
                ..Default::default()
            };
            return (defaults, false);
        }
        Err(error) => {
            eprintln!("failed to read settings, using defaults: {error}");
            return (AppSettings::default(), true);
        }
    };
    match serde_json::from_str(&text) {
        Ok(settings) => (settings, true),
        Err(error) => {
            eprintln!("settings file is corrupt, ignoring: {error}");
            (AppSettings::default(), true)
        }
    }
}

/// Read the legacy `aliases.json` (a flat `{ "word": "path" }` object) into settings-shaped
/// entries. Absent or malformed file → no entries (a broken legacy file must not block).
fn migrate_legacy_aliases(app_data_dir: &Path) -> Vec<AliasEntry> {
    let Ok(bytes) = fs::read(app_data_dir.join(LEGACY_ALIASES_FILE)) else {
        return Vec::new();
    };
    let Ok(raw) = serde_json::from_slice::<BTreeMap<String, String>>(&bytes) else {
        return Vec::new();
    };
    raw.into_iter()
        .map(|(word, path)| AliasEntry { word, path })
        .collect()
}

/// Persist settings atomically: write a sibling temp file, then rename over the target, so a
/// crash mid-write can never leave a half-written file (matches `pinned_tabs.rs`).
pub fn write(path: &Path, settings: &AppSettings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create app data dir: {error}"))?;
    }
    let json = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("failed to encode settings: {error}"))?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, json.as_bytes())
        .map_err(|error| format!("failed to write settings: {error}"))?;
    fs::rename(&temp_path, path).map_err(|error| format!("failed to replace settings: {error}"))
}

/// Read the whole settings surface (SPEC §12). The frontend loads this once on startup and
/// re-pulls on the `beeline://settings-changed` event.
#[tauri::command]
pub fn get_settings(app: AppHandle) -> Result<AppSettings, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("failed to resolve app data dir: {error}"))?;
    Ok(read(&dir).0)
}

/// The settings path for a live [`AppHandle`], used by the `set_settings` orchestration in
/// `lib.rs` (which also re-registers the shortcut and refreshes the Name Index).
pub fn path_for(app: &AppHandle) -> Result<PathBuf, String> {
    settings_path(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults_and_flags_first_run() {
        let dir = std::env::temp_dir().join(format!("beeline_settings_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (settings, existed) = read(&dir);
        assert!(!existed);
        assert!(!settings.first_run_dismissed);
        assert_eq!(settings.default_entry_point, EntryPoint::Recents);
        assert_eq!(
            settings.temporary_tab_lifetime,
            Lifetime::Minutes { minutes: 180 }
        );
        assert!(settings.preview_panel_visible);
        assert_eq!(
            settings.after_action.get("open_file").map(String::as_str),
            Some("hide")
        );
        assert_eq!(
            settings.after_action.get("trash").map(String::as_str),
            Some("keep")
        );
        assert!(!settings.junk_patterns.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("beeline_settings_rt_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        let settings = AppSettings {
            preview_panel_visible: false,
            first_run_dismissed: true,
            aliases: vec![AliasEntry {
                word: "docs".to_owned(),
                path: "~/Documents".to_owned(),
            }],
            ..Default::default()
        };
        write(&path, &settings).unwrap();
        let (loaded, existed) = read(&dir);
        assert!(existed);
        assert_eq!(loaded, settings);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn absorbs_legacy_aliases_on_first_run() {
        let dir = std::env::temp_dir().join(format!("beeline_settings_mig_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(LEGACY_ALIASES_FILE),
            r#"{"загрузки":"~/Downloads"}"#.as_bytes(),
        )
        .unwrap();
        let (settings, existed) = read(&dir);
        assert!(!existed);
        assert_eq!(settings.aliases.len(), 1);
        assert_eq!(settings.aliases[0].word, "загрузки");
        assert_eq!(settings.aliases[0].path, "~/Downloads");
        let _ = fs::remove_dir_all(&dir);
    }
}
