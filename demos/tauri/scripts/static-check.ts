#!/usr/bin/env bun

const app = await Bun.file("src/App.tsx").text();
const backend = await Bun.file("src-tauri/src/lib.rs").text();
const config = await Bun.file("src-tauri/tauri.conf.json").json();

const requiredEvents = [
  "process_start",
  "backend_ready",
  "frontend_ready",
  "shortcut_received",
  "show_requested",
  "window_visible",
  "path_submitted",
  "directory_loaded",
  "recents_loaded",
  "benchmark_10k_rendered",
  "quick_look_requested",
  "window_hidden",
];

const combined = `${app}\n${backend}`;
const missing = requiredEvents.filter((event) => !combined.includes(`\"${event}\"`));
if (missing.length) throw new Error(`Missing required events: ${missing.join(", ")}`);
if (config.app.windows[0].visible !== false) throw new Error("Benchmark window must start hidden");
if (config.app.windows[0].width !== 920 || config.app.windows[0].height !== 640) {
  throw new Error("Benchmark window must be 920x640");
}
if (!backend.includes("DEFAULT_SHORTCUT: &str = \"CommandOrControl+Shift+F\"")) {
  throw new Error("Default shortcut changed");
}
if (!backend.includes('env::var("VISUAL_FILES_START_VISIBLE").as_deref() == Ok("1")')) {
  throw new Error("Test-only visible launch hook is missing");
}
if (!backend.includes('show_main_window(&app, "test_hook")')) {
  throw new Error("Visible launch hook does not use the normal show path");
}
if (!app.includes('invoke("frontend_ready")')) {
  throw new Error("Frontend does not notify the prewarmed backend");
}
if (!app.includes("10_000")) throw new Error("10k deterministic mode is missing");

console.log("Static benchmark contract check passed.");
