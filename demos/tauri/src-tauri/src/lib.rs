use serde::Serialize;
use serde_json::{json, Map, Value};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    str::FromStr,
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, State, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

const DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+F";

struct EventLogger {
    start: Instant,
    file: Mutex<File>,
}

impl EventLogger {
    fn new(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            start: Instant::now(),
            file: Mutex::new(file),
        })
    }

    fn log(&self, event: &str, fields: Value) {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64() * 1_000.0)
            .unwrap_or(0.0);
        let mut record = Map::new();
        record.insert("event".into(), Value::String(event.into()));
        record.insert(
            "monotonic_ms".into(),
            json!(self.start.elapsed().as_secs_f64() * 1_000.0),
        );
        record.insert("unix_ms".into(), json!(unix_ms));
        if let Value::Object(fields) = fields {
            record.extend(fields);
        }
        if let Ok(mut file) = self.file.lock() {
            if serde_json::to_writer(&mut *file, &record).is_ok() {
                let _ = file.write_all(b"\n");
                let _ = file.flush();
            }
        }
    }
}

struct AppState {
    logger: EventLogger,
    quick_look: Mutex<Option<Child>>,
}

#[derive(Clone, Serialize)]
struct FileItem {
    path: String,
    name: String,
    kind: String,
    modified_ms: Option<f64>,
    size: Option<u64>,
    is_dir: bool,
}

#[derive(Serialize)]
struct LoadResult {
    location: String,
    items: Vec<FileItem>,
    selected_path: Option<String>,
    duration_ms: f64,
}

fn metadata_millis(metadata: &fs::Metadata) -> Option<f64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs_f64() * 1_000.0)
}

fn item_from_path(path: PathBuf) -> Option<FileItem> {
    let metadata = fs::metadata(&path).ok()?;
    let is_dir = metadata.is_dir();
    let name = path.file_name()?.to_string_lossy().into_owned();
    let kind = if is_dir {
        "Folder".to_string()
    } else {
        path.extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.is_empty())
            .map(|extension| extension.to_uppercase())
            .unwrap_or_else(|| "File".to_string())
    };
    Some(FileItem {
        path: path.to_string_lossy().into_owned(),
        name,
        kind,
        modified_ms: metadata_millis(&metadata),
        size: (!is_dir).then_some(metadata.len()),
        is_dir,
    })
}

fn expand_path(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Path is empty".into());
    }
    if trimmed == "~" || trimmed.starts_with("~/") {
        let home = env::var_os("HOME").ok_or("HOME is not available")?;
        let suffix = trimmed.strip_prefix("~/").unwrap_or("");
        Ok(PathBuf::from(home).join(suffix))
    } else {
        Ok(PathBuf::from(trimmed))
    }
}

fn enumerate_directory(path: &Path) -> Result<Vec<FileItem>, String> {
    let mut items = fs::read_dir(path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| item_from_path(entry.path()))
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(items)
}

fn load_path_sync(input: String) -> Result<LoadResult, String> {
    let started = Instant::now();
    let requested = expand_path(&input)?;
    let canonical = requested
        .canonicalize()
        .map_err(|error| format!("Cannot resolve {}: {error}", requested.display()))?;
    let (directory, selected_path) = if canonical.is_file() {
        let parent = canonical
            .parent()
            .ok_or("File has no parent directory")?
            .to_path_buf();
        (parent, Some(canonical.to_string_lossy().into_owned()))
    } else if canonical.is_dir() {
        (canonical, None)
    } else {
        return Err(format!(
            "{} is not a file or directory",
            canonical.display()
        ));
    };
    let items = enumerate_directory(&directory)?;
    Ok(LoadResult {
        location: directory.to_string_lossy().into_owned(),
        items,
        selected_path,
        duration_ms: started.elapsed().as_secs_f64() * 1_000.0,
    })
}

fn load_recents_sync() -> Result<LoadResult, String> {
    let started = Instant::now();
    let query =
        "kMDItemLastUsedDate >= $time.today(-90) && kMDItemContentTypeTree == 'public.item'";
    let output = Command::new("/usr/bin/mdfind")
        .args(["-0", query])
        .output()
        .map_err(|error| format!("Could not start mdfind: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "mdfind failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut items = String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
        .take(500)
        .filter_map(|path| item_from_path(PathBuf::from(path)))
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .modified_ms
            .partial_cmp(&left.modified_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(LoadResult {
        location: "Recents".into(),
        items,
        selected_path: None,
        duration_ms: started.elapsed().as_secs_f64() * 1_000.0,
    })
}

#[tauri::command]
async fn load_path(input: String, state: State<'_, AppState>) -> Result<LoadResult, String> {
    state.logger.log("path_submitted", json!({ "path": input }));
    let result = tauri::async_runtime::spawn_blocking(move || load_path_sync(input))
        .await
        .map_err(|error| error.to_string())??;
    state.logger.log(
        "directory_loaded",
        json!({
            "duration_ms": result.duration_ms,
            "item_count": result.items.len(),
            "location": result.location,
        }),
    );
    Ok(result)
}

#[tauri::command]
async fn load_recents(state: State<'_, AppState>) -> Result<LoadResult, String> {
    let result = tauri::async_runtime::spawn_blocking(load_recents_sync)
        .await
        .map_err(|error| error.to_string())??;
    state.logger.log(
        "recents_loaded",
        json!({ "duration_ms": result.duration_ms, "item_count": result.items.len() }),
    );
    Ok(result)
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    Command::new("/usr/bin/open")
        .arg(&path)
        .spawn()
        .map_err(|error| format!("Could not open {path}: {error}"))?;
    Ok(())
}

#[tauri::command]
fn copy_path(path: String) -> Result<(), String> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not start pbcopy: {error}"))?;
    child
        .stdin
        .take()
        .ok_or("pbcopy stdin was unavailable")?
        .write_all(path.as_bytes())
        .map_err(|error| error.to_string())?;
    child.wait().map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn toggle_quick_look(path: String, state: State<'_, AppState>) -> Result<(), String> {
    state
        .logger
        .log("quick_look_requested", json!({ "path": path }));
    let mut current = state.quick_look.lock().map_err(|error| error.to_string())?;
    if let Some(mut child) = current.take() {
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            child.kill().map_err(|error| error.to_string())?;
            return Ok(());
        }
    }
    if fs::metadata(&path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        let child = Command::new("/usr/bin/qlmanage")
            .args(["-p", &path])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Could not start Quick Look: {error}"))?;
        *current = Some(child);
    }
    Ok(())
}

#[tauri::command]
fn hide_window(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|error| error.to_string())?;
        state
            .logger
            .log("window_hidden", json!({ "source": "escape" }));
    }
    Ok(())
}

#[tauri::command]
fn log_frontend_event(name: String, fields: Value, state: State<'_, AppState>) {
    state.logger.log(&name, fields);
}

fn show_main_window(app: &tauri::AppHandle, origin: &str) {
    let state = app.state::<AppState>();
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    state
        .logger
        .log("show_requested", json!({ "origin": origin }));
    let _ = window.center();
    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit("focus-path-input", ());
    state.logger.log(
        "window_visible",
        json!({ "origin": origin, "visible": window.is_visible().unwrap_or(false) }),
    );
}

#[tauri::command]
fn frontend_ready(app: tauri::AppHandle, state: State<'_, AppState>) {
    state.logger.log("frontend_ready", json!({}));
    if env::var("VISUAL_FILES_START_VISIBLE").as_deref() == Ok("1") {
        show_main_window(&app, "test_hook");
    }
}

fn show_or_hide(app: &tauri::AppHandle, shortcut_name: &str) {
    let state = app.state::<AppState>();
    state
        .logger
        .log("shortcut_received", json!({ "shortcut": shortcut_name }));
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        state
            .logger
            .log("window_hidden", json!({ "source": "shortcut" }));
        return;
    }
    show_main_window(app, "shortcut");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            load_path,
            load_recents,
            open_path,
            copy_path,
            toggle_quick_look,
            hide_window,
            log_frontend_event,
            frontend_ready,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let log_path = app
                .path()
                .app_data_dir()
                .map_err(|error| error.to_string())?
                .join("benchmark.ndjson");
            let logger = EventLogger::new(&log_path)?;
            logger.log("process_start", json!({ "pid": std::process::id() }));
            app.manage(AppState {
                logger,
                quick_look: Mutex::new(None),
            });

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_visible_on_all_workspaces(true);
            }

            // Headless benchmark hook: run backend loads with the window hidden,
            // log the normal events with origin "headless", then exit. Tasks are
            // semicolon-separated paths; the token "recents" runs a Recents query.
            if let Ok(bench) = env::var("VISUAL_FILES_HEADLESS_BENCH") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let state = handle.state::<AppState>();
                    for task in bench.split(';').filter(|t| !t.is_empty()) {
                        if task == "recents" {
                            match load_recents_sync() {
                                Ok(result) => state.logger.log(
                                    "recents_loaded",
                                    json!({
                                        "duration_ms": result.duration_ms,
                                        "item_count": result.items.len(),
                                        "origin": "headless",
                                    }),
                                ),
                                Err(error) => state
                                    .logger
                                    .log("headless_bench_error", json!({ "task": task, "error": error })),
                            }
                            continue;
                        }
                        match load_path_sync(task.to_string()) {
                            Ok(result) => state.logger.log(
                                "directory_loaded",
                                json!({
                                    "duration_ms": result.duration_ms,
                                    "item_count": result.items.len(),
                                    "location": result.location,
                                    "origin": "headless",
                                }),
                            ),
                            Err(error) => state
                                .logger
                                .log("headless_bench_error", json!({ "task": task, "error": error })),
                        }
                    }
                    state.logger.log("headless_bench_done", json!({}));
                    std::process::exit(0);
                });
            }

            let shortcut_name =
                env::var("VISUAL_FILES_SHORTCUT").unwrap_or_else(|_| DEFAULT_SHORTCUT.to_string());
            let shortcut = Shortcut::from_str(&shortcut_name).map_err(|error| {
                format!("Invalid VISUAL_FILES_SHORTCUT {shortcut_name:?}: {error}")
            })?;
            let callback_shortcut = shortcut_name.clone();
            app.global_shortcut()
                .on_shortcut(shortcut, move |app, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        show_or_hide(app, &callback_shortcut);
                    }
                })?;

            let state = app.state::<AppState>();
            state.logger.log(
                "backend_ready",
                json!({ "shortcut": shortcut_name, "log_path": log_path }),
            );
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                let state = window.state::<AppState>();
                state
                    .logger
                    .log("window_hidden", json!({ "source": "close_requested" }));
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Tauri benchmark");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_path() {
        let home = env::var("HOME").expect("HOME is set for the test");
        assert_eq!(
            expand_path("~/Downloads").unwrap(),
            PathBuf::from(home).join("Downloads")
        );
    }

    #[test]
    fn generated_shortcut_parses() {
        assert!(Shortcut::from_str(DEFAULT_SHORTCUT).is_ok());
    }

    #[test]
    fn lists_a_real_directory() {
        let result = load_path_sync("/tmp".into()).expect("/tmp can be listed");
        assert_eq!(
            result.location,
            PathBuf::from("/tmp")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
        );
    }
}
