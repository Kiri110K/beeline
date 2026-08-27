mod listing;
mod name_index;
mod operations;
mod pinned_tabs;
mod power;
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

use listing::{
    list_location_file_neighbor, list_location_initial, list_location_selection_paths,
    list_location_window, validate_directory, ListingSessions,
};
use name_index::{record_visit, search_name_index, NameIndex};
use operations::{
    cancel_operation, create_folder, delete_items_permanently, open_in_app, paste_copy, paste_move,
    rename_item, resolve_installed_bundle, reveal_in_finder, trash_items, Operations,
};
use pinned_tabs::{load_pinned_tabs, save_pinned_tabs};
use preview::{preview_metadata, preview_text_excerpt, preview_thumbnail, ThumbnailTracker};
use quick_look::{
    prewarm as prewarm_quick_look, quick_look_hide, quick_look_is_open, quick_look_show,
    quick_look_update, QuickLook,
};
use recents::{get_recents, RecentsCache};
use settings::get_settings;
use telemetry::Telemetry;

const MAIN_WINDOW_LABEL: &str = "main";
const WINDOW_SHOWN_EVENT: &str = "beeline://window-shown";
const SETTINGS_CHANGED_EVENT: &str = "beeline://settings-changed";
const SNAP_DISTANCE_PX: u64 = 12;
/// The wider radius inside which the frontend shows the center guide lines (SPEC §2).
const GUIDE_DISTANCE_PX: u64 = 48;
const CENTER_GUIDE_EVENT: &str = "beeline://center-guide";

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
    // Monotonic token bumped by every show; a settle loop keeps its token and
    // bails when a newer show has taken over (see `spawn_focus_settle`).
    settle_generation: AtomicU64,
}

impl ShellState {
    fn new(show_on_frontend_ready: bool) -> Self {
        Self {
            launch_show_claimed: AtomicBool::new(false),
            next_show_sequence: AtomicU64::new(1),
            show_on_frontend_ready,
            last_hidden_at_ms: Mutex::new(None),
            settle_generation: AtomicU64::new(0),
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
    // Current macOS treats activation as a cooperative request. An Accessory app may be
    // activated in principle, but a global-shortcut callback has repeatedly been denied on
    // the reference machine. Become a Regular app for the visible session, then return to
    // Accessory in the single hide path so the hidden resident has no Dock presence.
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Regular)
        .map_err(|error| format!("failed to enable visible activation policy: {error}"))?;
    window
        .show()
        .map_err(|error| format!("failed to show main window: {error}"))?;
    activate_and_make_key(app, &window);
    // Keep the tauri-level focus call so the plugin's internal focus state matches
    // AppKit's; the settle loop below is what actually makes it stick (§2).
    window
        .set_focus()
        .map_err(|error| format!("failed to focus main window: {error}"))?;
    spawn_focus_settle(app, &window, state);

    let hidden_ms = state.take_hidden_ms();
    let window_key = window.is_focused().unwrap_or(false);
    let app_active = application_is_active();
    record_event(
        app,
        "window_visible",
        json!({
            "focused": window_key && app_active,
            "window_key": window_key,
            "app_active": app_active,
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

// The single window must move to whichever Space owns the shortcut, including over
// another app's full-screen Space. A hidden `CanJoinAllSpaces` window remained attached
// to its previous desktop on the reference Mac: activation switched desktops while the
// window stayed absent. `MoveToActiveSpace` gives the launcher behavior we need, while
// `FullScreenAuxiliary` permits that move onto a full-screen Space (§10).
fn install_space_behavior(window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWindowCollectionBehavior;
        if let Ok(ns_window_ptr) = window.ns_window() {
            // SAFETY: tauri hands back the live NSWindow pointer for this window; we
            // only set its collection behavior on the main thread (setup runs there).
            let ns_window = unsafe { &*ns_window_ptr.cast::<objc2_app_kit::NSWindow>() };
            ns_window.setCollectionBehavior(
                NSWindowCollectionBehavior::MoveToActiveSpace
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

// Number of settle retries and the gap between them: since macOS 14 activation is
// cooperative, so a single request from a non-frontmost Accessory app may be dropped.
// The 20 ms retry stays inside the 50 ms warm-entry budget when one retry is needed.
const FOCUS_SETTLE_ATTEMPTS: u32 = 8;
const FOCUS_SETTLE_INTERVAL_MS: u64 = 20;

// An Accessory app that has never been active does not come frontmost from `set_focus`
// alone. Drive the full AppKit sequence on the main thread: make the window key, then
// use macOS 14's user-intent-aware activation request. The old
// `activateIgnoringOtherApps` API is deprecated and has no effect on current macOS.
// Best-effort: a dropped request is what `spawn_focus_settle` retries.
fn activate_and_make_key(app: &AppHandle, window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread({
            let window = window.clone();
            move || {
                let Some(marker) = objc2_foundation::MainThreadMarker::new() else {
                    return;
                };
                let ns_app = objc2_app_kit::NSApplication::sharedApplication(marker);
                if let Ok(ns_window_ptr) = window.ns_window() {
                    // SAFETY: tauri hands back the live NSWindow pointer for this window;
                    // ns_window() and these calls all run on the main thread (this closure).
                    let ns_window = unsafe { &*ns_window_ptr.cast::<objc2_app_kit::NSWindow>() };
                    ns_window.orderFrontRegardless();
                    ns_window.makeKeyAndOrderFront(None);
                }
                ns_app.activate();
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, window);
}

// A window can be "key" inside an inactive Accessory app while another process still owns
// the keyboard. NSRunningApplication is thread-safe and reports the OS-level frontmost
// truth, so every focus decision combines both signals.
#[cfg(target_os = "macos")]
fn application_is_active() -> bool {
    objc2_app_kit::NSRunningApplication::currentApplication().isActive()
}

#[cfg(not(target_os = "macos"))]
fn application_is_active() -> bool {
    true
}

fn has_foreground_focus(window: &WebviewWindow) -> (bool, bool, bool) {
    let window_key = window.is_focused().unwrap_or(false);
    let app_active = application_is_active();
    (window_key && app_active, window_key, app_active)
}

// Activation may still be deferred after the initial request, so verify and retry
// off the show path. Every 20 ms, up to 8 times: if the window is key, stop; else
// re-run the activation sequence. Exactly one `focus_settled` event is recorded
// (attempts made, final focus). A generation token guards against overlapping loops
// from rapid re-shows — a newer show bumps the token, and this stale loop exits
// silently without recording so only the current show reports.
fn spawn_focus_settle(app: &AppHandle, window: &WebviewWindow, state: &ShellState) {
    let generation = state.settle_generation.fetch_add(1, Ordering::AcqRel) + 1;
    let app = app.clone();
    let window = window.clone();
    std::thread::spawn(move || {
        let state = app.state::<ShellState>();
        let mut attempts = 0;
        let (mut focused, mut window_key, mut app_active) = has_foreground_focus(&window);
        while !focused && attempts < FOCUS_SETTLE_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(FOCUS_SETTLE_INTERVAL_MS));
            if state.settle_generation.load(Ordering::Acquire) != generation {
                return;
            }
            activate_and_make_key(&app, &window);
            attempts += 1;
            (focused, window_key, app_active) = has_foreground_focus(&window);
        }
        if state.settle_generation.load(Ordering::Acquire) != generation {
            return;
        }
        record_event(
            &app,
            "focus_settled",
            json!({
                "attempts": attempts,
                "focused": focused,
                "window_key": window_key,
                "app_active": app_active,
            }),
        );
    });
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
    // Invalidate any in-flight focus-settle loop: its retries call
    // `orderFrontRegardless`, which would re-show a window hidden within the
    // settle window (~320 ms after a show).
    state.settle_generation.fetch_add(1, Ordering::AcqRel);
    state.stamp_hidden();
    window
        .hide()
        .map_err(|error| format!("failed to hide main window: {error}"))?;
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory)
        .map_err(|error| format!("failed to restore hidden activation policy: {error}"))?;
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
    let (is_focused, _, _) = has_foreground_focus(&window);

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

/// Snap when within [`SNAP_DISTANCE_PX`] of the centered position; report whether the
/// window is within the wider guide radius so the frontend can draw the center guide
/// lines (SPEC §2: guide lines give a snap target back to center).
fn snap_to_center_if_near(
    window: &WebviewWindow,
    position: PhysicalPosition<i32>,
) -> tauri::Result<bool> {
    let Some(monitor) = window.current_monitor()? else {
        return Ok(false);
    };
    let window_size = window.outer_size()?;
    let work_area = monitor.work_area();
    let centered_x = i64::from(work_area.position.x)
        + (i64::from(work_area.size.width) - i64::from(window_size.width)) / 2;
    let centered_y = i64::from(work_area.position.y)
        + (i64::from(work_area.size.height) - i64::from(window_size.height)) / 2;

    let dx = i64::from(position.x).abs_diff(centered_x);
    let dy = i64::from(position.y).abs_diff(centered_y);
    if dx <= SNAP_DISTANCE_PX && dy <= SNAP_DISTANCE_PX {
        window.center()?;
    }
    Ok(dx <= GUIDE_DISTANCE_PX && dy <= GUIDE_DISTANCE_PX)
}

// The close button must never destroy the single window (SPEC §2: exactly one
// application window, ever) — it hides through the one hide path instead, so the
// background clock is stamped and the shortcut can summon the window back.
fn install_close_to_hide(window: &WebviewWindow) {
    let target = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let app = target.app_handle();
            let state = app.state::<ShellState>();
            if let Err(error) = perform_hide(app, &target, &state, "close-button") {
                eprintln!("close-button hide failed: {error}");
            }
        }
    });
}

fn install_center_snap(window: &WebviewWindow) {
    let snap_target = window.clone();
    let was_near = AtomicBool::new(false);
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(position) = event {
            match snap_to_center_if_near(&snap_target, *position) {
                // Emit on every near tick (the frontend's hide timeout re-arms on each)
                // and once on leaving, so the guides cannot stick after the drag ends.
                Ok(true) => {
                    was_near.store(true, Ordering::Relaxed);
                    let _ = snap_target.emit(CENTER_GUIDE_EVENT, true);
                }
                Ok(false) => {
                    if was_near.swap(false, Ordering::Relaxed) {
                        let _ = snap_target.emit(CENTER_GUIDE_EVENT, false);
                    }
                }
                Err(error) => eprintln!("window center snap failed: {error}"),
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
            list_location_file_neighbor,
            list_location_initial,
            list_location_selection_paths,
            list_location_window,
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
            trash_items,
            validate_directory
        ])
        .setup(move |app| {
            // Replace tauri's default macOS menu with a minimal one. The default File >
            // Close Window swallowed Cmd+W before the webview (observed live: Cmd+W hid
            // the window through CloseRequested instead of closing the Tab, §4). Removing
            // the menu entirely is worse: a menu-less app can never own the menu bar, so
            // macOS refuses to make it the active application (observed live too). So:
            // an app submenu whose Quit has NO accelerator (a hidden resident must not
            // die to a stray Cmd+Q; Quit lives in the Action Menu, §5/§12) plus a
            // standard Edit submenu so text-field Cmd+C/V/A stay reliable on any layout.
            {
                use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
                let quit = MenuItemBuilder::with_id("quit", "Quit Beeline").build(app)?;
                let app_submenu = SubmenuBuilder::new(app, "Beeline").item(&quit).build()?;
                let edit = SubmenuBuilder::new(app, "Edit")
                    .undo()
                    .redo()
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .build()?;
                let menu = MenuBuilder::new(app)
                    .items(&[&app_submenu, &edit])
                    .build()?;
                app.set_menu(menu)?;
                app.on_menu_event(|app, event| {
                    if event.id() == "quit" {
                        app.exit(0);
                    }
                });
            }
            let telemetry = Telemetry::new(&app.path().app_data_dir()?, process_started)?;
            telemetry.record(
                "process_start",
                json!({ "launch_mode": if hidden_launch { "hidden" } else { "manual" } }),
            )?;
            app.manage(telemetry);
            app.manage(ShellState::new(!hidden_launch));
            app.manage(Operations::new());
            app.manage(QuickLook::new(app.handle()));
            if let Err(error) = prewarm_quick_look(app.handle()) {
                eprintln!("Quick Look prewarm dispatch failed: {error}");
            }
            app.manage(ThumbnailTracker::new());
            // Load the last-success Recents cache so the first get_recents paints from it
            // instantly; the refresh is triggered lazily by that first call (§7, §11).
            app.manage(RecentsCache::load(&app.path().app_data_dir()?));
            app.manage(ListingSessions::new());

            let window = app
                .get_webview_window(MAIN_WINDOW_LABEL)
                .ok_or("main window is missing")?;
            install_center_snap(&window);
            install_space_behavior(&window);
            install_close_to_hide(&window);

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
        match event {
            // Persist the Name Index on graceful shutdown (never periodically, §11).
            tauri::RunEvent::Exit => {
                if let Some(name_index) = app_handle.try_state::<NameIndex>() {
                    name_index.persist();
                }
            }
            // Opening the app again (Finder, `open`, Dock) reopens the hidden
            // resident: show and focus the single window (§2).
            tauri::RunEvent::Reopen { .. } => {
                let state = app_handle.state::<ShellState>();
                if let Err(error) = show_and_focus(app_handle, &state, ShowOrigin::Launch) {
                    eprintln!("reopen show failed: {error}");
                }
            }
            _ => {}
        }
    });
}
