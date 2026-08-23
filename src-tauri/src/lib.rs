mod listing;
mod name_index;
mod operations;
mod pinned_tabs;
mod preview;
mod qos;
mod quick_look;
mod recents;
mod settings;
mod telemetry;

use std::{
    env,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::{json, Map, Value};
use tauri::{
    plugin::TauriPlugin, AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindow,
    WindowEvent, Wry,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use listing::list_location;
use name_index::{record_visit, search_name_index, NameIndex};
use operations::{
    cancel_operation, create_folder, delete_items_permanently, open_in_app, paste_copy, paste_move,
    rename_item, resolve_installed_bundle, reveal_in_finder, trash_items, Operations,
};
use pinned_tabs::{load_pinned_tabs, save_pinned_tabs};
use preview::{preview_metadata, preview_text_excerpt, preview_thumbnail, ThumbnailTracker};
use quick_look::{
    quick_look_hide, quick_look_is_open, quick_look_show, quick_look_update, QuickLook,
};
use recents::{get_recents, RecentsCache};
use settings::get_settings;
use telemetry::Telemetry;

const MAIN_WINDOW_LABEL: &str = "main";
const WINDOW_SHOWN_EVENT: &str = "beeline://window-shown";
const SETTINGS_CHANGED_EVENT: &str = "beeline://settings-changed";
const SNAP_DISTANCE_PX: u64 = 12;

// Wall-clock ms since the Unix epoch, used to measure continuous background time
// (which must count sleep, so a monotonic clock will not do — §2, §9).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// Build a global-shortcut `Shortcut` from the persisted spec (SPEC §2). The spec's `code`
// is a W3C `KeyboardEvent.code` (`"KeyF"`), which `Code::from_str` accepts verbatim, so the
// frontend capture field and the plugin share one representation. `None` when the code is
// unrecognized (a malformed spec keeps the old shortcut, see `set_settings`).
fn to_shortcut(spec: &settings::ShortcutSpec) -> Option<Shortcut> {
    let code = Code::from_str(&spec.code).ok()?;
    let mut modifiers = Modifiers::empty();
    if spec.control {
        modifiers |= Modifiers::CONTROL;
    }
    if spec.alt {
        modifiers |= Modifiers::ALT;
    }
    if spec.shift {
        modifiers |= Modifiers::SHIFT;
    }
    if spec.meta {
        modifiers |= Modifiers::SUPER;
    }
    Some(Shortcut::new(Some(modifiers), code))
}

/// The result of a settings write (SPEC §12): whether the global shortcut is registered.
/// `false` means the new shortcut could not be claimed and the old one was kept, so the
/// frontend surfaces a Status Strip problem and the persisted shortcut stays unchanged.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetSettingsOutcome {
    shortcut_registered: bool,
}

// Persist the whole settings surface (SPEC §12) and apply its live consumers: re-register
// the global shortcut when it changed, refresh the Name Index's Junk patterns and aliases,
// and emit `beeline://settings-changed` so every frontend consumer re-pulls. A shortcut that
// cannot be registered is rolled back to the previous one, which stays live.
#[tauri::command]
fn set_settings(
    app: AppHandle,
    settings: settings::AppSettings,
) -> Result<SetSettingsOutcome, String> {
    let path = settings::path_for(&app)?;
    let dir = path
        .parent()
        .ok_or_else(|| "settings path has no parent".to_owned())?;
    let (old, _) = settings::read(dir);
    let mut next = settings;

    let mut shortcut_registered = true;
    if next.global_shortcut != old.global_shortcut {
        shortcut_registered =
            reregister_shortcut(&app, &old.global_shortcut, &next.global_shortcut);
        if !shortcut_registered {
            // Keep the old shortcut both live (handled in reregister_shortcut) and persisted.
            next.global_shortcut = old.global_shortcut.clone();
        }
    }

    settings::write(&path, &next)?;

    if let Some(index) = app.try_state::<NameIndex>() {
        let aliases = next
            .aliases
            .iter()
            .map(|entry| (entry.word.clone(), entry.path.clone()))
            .collect();
        index.apply_settings(next.junk_patterns.clone(), aliases);
    }

    app.emit(SETTINGS_CHANGED_EVENT, ())
        .map_err(|error| format!("failed to emit settings changed event: {error}"))?;

    Ok(SetSettingsOutcome {
        shortcut_registered,
    })
}

// Swap the live global shortcut: unregister the old, register the new, and on failure put
// the old one back so the app is never left without a working shortcut (SPEC §2). Returns
// whether the new shortcut is now registered.
fn reregister_shortcut(
    app: &AppHandle,
    old: &settings::ShortcutSpec,
    new: &settings::ShortcutSpec,
) -> bool {
    let Some(new_shortcut) = to_shortcut(new) else {
        eprintln!("new global shortcut spec is invalid; keeping the old one");
        return false;
    };
    let shortcuts = app.global_shortcut();
    if let Some(old_shortcut) = to_shortcut(old) {
        let _ = shortcuts.unregister(old_shortcut);
    }
    match shortcuts.register(new_shortcut) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("global shortcut re-registration failed: {error}");
            if let Some(old_shortcut) = to_shortcut(old) {
                let _ = shortcuts.register(old_shortcut);
            }
            false
        }
    }
}

struct ShellState {
    launch_show_claimed: AtomicBool,
    next_show_sequence: AtomicU64,
    show_on_frontend_ready: bool,
    // Wall-clock ms of the last hide, cleared on the next show.
    last_hidden_at_ms: Mutex<Option<u64>>,
}

impl ShellState {
    fn new(show_on_frontend_ready: bool) -> Self {
        Self {
            launch_show_claimed: AtomicBool::new(false),
            next_show_sequence: AtomicU64::new(1),
            show_on_frontend_ready,
            last_hidden_at_ms: Mutex::new(None),
        }
    }

    fn next_show_sequence(&self) -> u64 {
        self.next_show_sequence.fetch_add(1, Ordering::Relaxed)
    }

    fn stamp_hidden(&self) {
        if let Ok(mut guard) = self.last_hidden_at_ms.lock() {
            *guard = Some(now_ms());
        }
    }

    // Continuous background ms since the last hide, consuming the stamp so a
    // second show without an intervening hide reports None.
    fn take_hidden_ms(&self) -> Option<u64> {
        let mut guard = self.last_hidden_at_ms.lock().ok()?;
        let hidden_at = guard.take()?;
        Some(now_ms().saturating_sub(hidden_at))
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum ShowOrigin {
    Launch,
    Shortcut,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowShownPayload {
    origin: ShowOrigin,
    sequence: u64,
    hidden_ms: Option<u64>,
}

fn record_event(app: &AppHandle, event: &str, fields: Value) {
    if let Err(error) = app.state::<Telemetry>().record(event, fields) {
        eprintln!("telemetry event {event} failed: {error}");
    }
}

fn show_and_focus(app: &AppHandle, state: &ShellState, origin: ShowOrigin) -> Result<(), String> {
    let sequence = state.next_show_sequence();
    record_event(
        app,
        "show_requested",
        json!({ "origin": origin, "sequence": sequence }),
    );

    let window = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "main window is missing".to_owned())?;
    window
        .show()
        .map_err(|error| format!("failed to show main window: {error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("failed to focus main window: {error}"))?;

    let hidden_ms = state.take_hidden_ms();
    record_event(
        app,
        "window_visible",
        json!({
            "focused": window.is_focused().unwrap_or(false),
            "origin": origin,
            "sequence": sequence,
            "visible": window.is_visible().unwrap_or(false),
            "hidden_ms": hidden_ms,
        }),
    );
    window
        .emit(
            WINDOW_SHOWN_EVENT,
            WindowShownPayload {
                origin,
                sequence,
                hidden_ms,
            },
        )
        .map_err(|error| format!("failed to emit window shown event: {error}"))
}

// The single hide path: stamp the background clock, hide, and record telemetry.
// `origin` distinguishes the global-shortcut toggle, an Escape, and a Pinned-Tab
// Cmd+W (§4, §9).
fn perform_hide(
    app: &AppHandle,
    window: &WebviewWindow,
    state: &ShellState,
    origin: &str,
) -> Result<(), String> {
    record_event(app, "hide_requested", json!({ "origin": origin }));
    state.stamp_hidden();
    window
        .hide()
        .map_err(|error| format!("failed to hide main window: {error}"))?;
    record_event(app, "window_hidden", json!({ "origin": origin }));
    Ok(())
}

fn toggle_main_window(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "main window is missing".to_owned())?;
    let is_visible = window
        .is_visible()
        .map_err(|error| format!("failed to read main window visibility: {error}"))?;
    let is_focused = window
        .is_focused()
        .map_err(|error| format!("failed to read main window focus: {error}"))?;

    let state = app.state::<ShellState>();
    if is_visible && is_focused {
        perform_hide(app, &window, &state, "shortcut")
    } else {
        show_and_focus(app, &state, ShowOrigin::Shortcut)
    }
}

// Quit the whole application (SPEC §5: the Action Menu's Quit). The resident is an
// accessory that normally hides rather than closing, so Quit is an explicit exit.
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn hide_window(app: AppHandle, state: State<'_, ShellState>, origin: String) -> Result<(), String> {
    let window = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "main window is missing".to_owned())?;
    perform_hide(&app, &window, &state, &origin)
}

// The global-shortcut plugin carries only the handler; the actual shortcut is registered at
// runtime from the persisted settings (SPEC §2, §12), so a Settings change can unregister
// and re-register it live without rebuilding the plugin.
fn global_shortcut_plugin() -> TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                if let Err(error) = toggle_main_window(app) {
                    eprintln!("global shortcut action failed: {error}");
                }
            }
        })
        .build()
}

fn snap_to_center_if_near(
    window: &WebviewWindow,
    position: PhysicalPosition<i32>,
) -> tauri::Result<()> {
    let Some(monitor) = window.current_monitor()? else {
        return Ok(());
    };
    let window_size = window.outer_size()?;
    let work_area = monitor.work_area();
    let centered_x = i64::from(work_area.position.x)
        + (i64::from(work_area.size.width) - i64::from(window_size.width)) / 2;
    let centered_y = i64::from(work_area.position.y)
        + (i64::from(work_area.size.height) - i64::from(window_size.height)) / 2;

    if i64::from(position.x).abs_diff(centered_x) <= SNAP_DISTANCE_PX
        && i64::from(position.y).abs_diff(centered_y) <= SNAP_DISTANCE_PX
    {
        window.center()?;
    }

    Ok(())
}

fn install_center_snap(window: &WebviewWindow) {
    let snap_target = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(position) = event {
            if let Err(error) = snap_to_center_if_near(&snap_target, *position) {
                eprintln!("window center snap failed: {error}");
            }
        }
    });
}

#[tauri::command]
fn frontend_ready(app: AppHandle, state: State<'_, ShellState>) -> Result<(), String> {
    if !state.show_on_frontend_ready {
        return Ok(());
    }

    if state
        .launch_show_claimed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }

    if let Err(error) = show_and_focus(&app, &state, ShowOrigin::Launch) {
        state.launch_show_claimed.store(false, Ordering::Release);
        return Err(error);
    }

    Ok(())
}

#[tauri::command]
fn home_directory(app: AppHandle) -> Result<String, String> {
    app.path()
        .home_dir()
        .map(|home| home.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn telemetry_event(
    name: String,
    fields: Map<String, Value>,
    telemetry: State<'_, Telemetry>,
) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("telemetry event name must not be empty".to_owned());
    }

    telemetry
        .record(&name, Value::Object(fields))
        .map_err(|error| format!("failed to record telemetry event: {error}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let process_started = Instant::now();
    let hidden_launch = env::args_os().any(|argument| argument == "--hidden");

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(global_shortcut_plugin())
        .invoke_handler(tauri::generate_handler![
            cancel_operation,
            create_folder,
            delete_items_permanently,
            frontend_ready,
            get_recents,
            get_settings,
            hide_window,
            home_directory,
            list_location,
            load_pinned_tabs,
            open_in_app,
            paste_copy,
            paste_move,
            preview_metadata,
            preview_text_excerpt,
            preview_thumbnail,
            quick_look_hide,
            quick_look_is_open,
            quick_look_show,
            quick_look_update,
            quit_app,
            record_visit,
            rename_item,
            resolve_installed_bundle,
            reveal_in_finder,
            save_pinned_tabs,
            search_name_index,
            set_settings,
            telemetry_event,
            trash_items
        ])
        .setup(move |app| {
            let telemetry = Telemetry::new(&app.path().app_data_dir()?, process_started)?;
            telemetry.record(
                "process_start",
                json!({ "launch_mode": if hidden_launch { "hidden" } else { "manual" } }),
            )?;
            app.manage(telemetry);
            app.manage(ShellState::new(!hidden_launch));
            app.manage(Operations::new());
            app.manage(QuickLook::new());
            app.manage(ThumbnailTracker::new());
            // Load the last-success Recents cache so the first get_recents paints from it
            // instantly; the refresh is triggered lazily by that first call (§7, §11).
            app.manage(RecentsCache::load(&app.path().app_data_dir()?));

            let window = app
                .get_webview_window(MAIN_WINDOW_LABEL)
                .ok_or("main window is missing")?;
            install_center_snap(&window);

            // Register the global shortcut from the persisted settings (SPEC §2, §12). The
            // plugin (with its handler) is already installed above; here we claim the actual
            // combination, so a Settings change can re-register it live.
            let (app_settings, _) = settings::read(&app.path().app_data_dir()?);
            let shortcut = to_shortcut(&app_settings.global_shortcut);
            let shortcut_registered = match &shortcut {
                Some(shortcut) => match app.global_shortcut().register(*shortcut) {
                    Ok(()) => true,
                    Err(error) => {
                        eprintln!("global shortcut registration failed: {error}");
                        false
                    }
                },
                None => {
                    eprintln!("persisted global shortcut spec is invalid");
                    false
                }
            };

            let telemetry = app.state::<Telemetry>();
            telemetry.record(
                "backend_ready",
                json!({
                    "log_path": telemetry.path().display().to_string(),
                    "shortcut": shortcut.map(|shortcut| shortcut.to_string()),
                    "shortcut_registered": shortcut_registered,
                }),
            )?;

            // The window is already created above; init only spawns background threads
            // and returns, so window show is never delayed (§10).
            match NameIndex::init(app.handle()) {
                Ok(name_index) => {
                    app.manage(name_index);
                }
                Err(error) => eprintln!("name index init failed: {error}"),
            }

            Ok(())
        });

    let mut app = builder
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    app.run(|app_handle, event| {
        // Persist the Name Index on graceful shutdown (never periodically, §11).
        if let tauri::RunEvent::Exit = event {
            if let Some(name_index) = app_handle.try_state::<NameIndex>() {
                name_index.persist();
            }
        }
    });
}
