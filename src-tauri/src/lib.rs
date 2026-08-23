mod telemetry;

use std::{
    env,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};

use serde::Serialize;
use serde_json::{json, Map, Value};
use tauri::{
    plugin::TauriPlugin, AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindow,
    WindowEvent, Wry,
};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

use telemetry::Telemetry;

const MAIN_WINDOW_LABEL: &str = "main";
const WINDOW_SHOWN_EVENT: &str = "beeline://window-shown";
const SNAP_DISTANCE_PX: u64 = 12;

fn default_shortcut() -> Shortcut {
    Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER),
        Code::KeyF,
    )
}

struct ShellState {
    launch_show_claimed: AtomicBool,
    next_show_sequence: AtomicU64,
    show_on_frontend_ready: bool,
}

impl ShellState {
    fn new(show_on_frontend_ready: bool) -> Self {
        Self {
            launch_show_claimed: AtomicBool::new(false),
            next_show_sequence: AtomicU64::new(1),
            show_on_frontend_ready,
        }
    }

    fn next_show_sequence(&self) -> u64 {
        self.next_show_sequence.fetch_add(1, Ordering::Relaxed)
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum ShowOrigin {
    Launch,
    Shortcut,
}

#[derive(Clone, Serialize)]
struct WindowShownPayload {
    origin: ShowOrigin,
    sequence: u64,
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

    record_event(
        app,
        "window_visible",
        json!({
            "focused": window.is_focused().unwrap_or(false),
            "origin": origin,
            "sequence": sequence,
            "visible": window.is_visible().unwrap_or(false),
        }),
    );
    window
        .emit(WINDOW_SHOWN_EVENT, WindowShownPayload { origin, sequence })
        .map_err(|error| format!("failed to emit window shown event: {error}"))
}

fn hide_for_shortcut(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    record_event(app, "hide_requested", json!({ "origin": "shortcut" }));
    window
        .hide()
        .map_err(|error| format!("failed to hide main window: {error}"))?;
    record_event(app, "window_hidden", json!({ "origin": "shortcut" }));
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

    if is_visible && is_focused {
        hide_for_shortcut(app, &window)
    } else {
        let state = app.state::<ShellState>();
        show_and_focus(app, &state, ShowOrigin::Shortcut)
    }
}

fn global_shortcut_plugin(
    shortcut: Shortcut,
) -> Result<TauriPlugin<Wry>, tauri_plugin_global_shortcut::Error> {
    Ok(tauri_plugin_global_shortcut::Builder::new()
        .with_shortcut(shortcut)?
        .with_handler(|app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                if let Err(error) = toggle_main_window(app) {
                    eprintln!("global shortcut action failed: {error}");
                }
            }
        })
        .build())
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
        .invoke_handler(tauri::generate_handler![frontend_ready, telemetry_event])
        .setup(move |app| {
            let telemetry = Telemetry::new(&app.path().app_data_dir()?, process_started)?;
            telemetry.record(
                "process_start",
                json!({ "launch_mode": if hidden_launch { "hidden" } else { "manual" } }),
            )?;
            app.manage(telemetry);
            app.manage(ShellState::new(!hidden_launch));

            let window = app
                .get_webview_window(MAIN_WINDOW_LABEL)
                .ok_or("main window is missing")?;
            install_center_snap(&window);

            let shortcut = default_shortcut();
            let shortcut_registration = global_shortcut_plugin(shortcut)
                .map_err(|error| error.to_string())
                .and_then(|plugin| {
                    app.handle()
                        .plugin(plugin)
                        .map_err(|error| error.to_string())
                });
            let shortcut_registered = shortcut_registration.is_ok();
            if let Err(error) = shortcut_registration {
                eprintln!("global shortcut registration failed: {error}");
            }

            let telemetry = app.state::<Telemetry>();
            telemetry.record(
                "backend_ready",
                json!({
                    "log_path": telemetry.path().display().to_string(),
                    "shortcut": shortcut.to_string(),
                    "shortcut_registered": shortcut_registered,
                }),
            )?;

            Ok(())
        });

    let mut app = builder
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    app.run(|_app_handle, _event| {});
}
