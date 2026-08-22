# Tauri host demo

This is the disposable Tauri v2 implementation of `../BENCHMARK.md`. It is not a production file manager.

The demo uses React and `@tanstack/react-virtual` in the WKWebView. Rust owns directory enumeration, Spotlight queries, file opening, path copying, Quick Look, the global shortcut, and the NDJSON event log. Directory and Recents loads run on blocking worker threads instead of the UI thread.

## Build and verify

Requirements:

- Bun 1.3 or newer
- stable Rust
- macOS Command Line Tools

```sh
bun install
bun run test
bun run build:app
```

The release bundle is written to:

```text
src-tauri/target/release/bundle/macos/Visual Files Tauri Benchmark.app
```

No development server is required or used.

## Run

Quit the Electron and AppKit demos first. The default shortcut is `Command+Shift+F`.

```sh
open "src-tauri/target/release/bundle/macos/Visual Files Tauri Benchmark.app"
```

For a smoke run that cannot collide with the other demos, start the executable directly with a temporary shortcut:

```sh
VISUAL_FILES_SHORTCUT=CommandOrControl+Shift+9 \
  "src-tauri/target/release/bundle/macos/Visual Files Tauri Benchmark.app/Contents/MacOS/visual-files-tauri-demo"
```

The environment override exists only for testing. A normal release run defaults to `Command+Shift+F`.

GUI automation can ask the prewarmed app to show itself immediately after React reports `frontend_ready`:

```sh
VISUAL_FILES_START_VISIBLE=1 VISUAL_FILES_SHORTCUT=CommandOrControl+Shift+9 \
  "src-tauri/target/release/bundle/macos/Visual Files Tauri Benchmark.app/Contents/MacOS/visual-files-tauri-demo"
```

This hook emits the normal `show_requested` and `window_visible` records with `origin: "test_hook"`, then focuses Path Input through the same window path used by the shortcut. Without the exact value `1`, startup stays hidden.

## Controls

- `Command+Shift+F` shows or hides the resident window.
- `Command+T` creates a Recents Tab and focuses Path Input.
- `Command+W` closes a Tab. The last Tab resets to Recents.
- Enter a directory, file, or `~` path in Path Input.
- Up, Down, `Ctrl+J`, and `Ctrl+K` change selection.
- Right Arrow or Enter enters a directory or opens a file.
- Left Arrow returns to the previous Location in the active Tab.
- Space toggles the Quick Look process for a real file.
- Single click selects. Double click performs the primary action. Context click offers Open, Copy path, and Quick Look.
- The `10k` button creates the deterministic virtualized list.
- Escape hides the window and leaves the process and Tabs alive.

Creating another Recents Tab triggers a fresh Spotlight query, which is the simplest repeatable Recents refresh for the benchmark.

## Event log

The app appends NDJSON records here:

```text
~/Library/Application Support/com.kiri.visual-files-tauri-benchmark/benchmark.ndjson
```

Summarize the default log or pass a saved log path:

```sh
bun run summarize
bun run summarize raw/hidden-startup-smoke.ndjson
```

The summarizer prints the sample count, median, p95, and slowest value for shortcut-to-visible, directory load, and Recents load events. Preserve each comparison run in `raw/` before starting the next run because the application log is append-only.

## Manual comparison procedure

1. Start the release build and wait for a `frontend_ready` event.
2. Press the shortcut 30 times at a steady pace. Confirm that Path Input has focus after every show.
3. Paste the same test directory five times. Record each `directory_loaded` event.
4. Create and close five Recents Tabs. Record each `recents_loaded` event.
5. Open `10k`, hold Down, then use the mouse and trackpad for continuous scrolling.
6. Repeat window activation from another Space, another monitor, a full-screen app, Russian and English layouts, and after sleep/wake.
7. Run ten rapid shortcut presses and check that the final state matches the number of presses.
8. Measure the resident process with Activity Monitor after two hidden minutes and again while scrolling `10k`.

Copy the raw log and measurements into `RESULTS.md`. Do not compare development builds.

## Known host-specific choices

- The app uses Tauri's `Accessory` activation policy and keeps one hidden WebviewWindow alive.
- `set_visible_on_all_workspaces(true)` uses Tauri's macOS private API feature. Full-screen and focus behavior still needs a real GUI run.
- A hidden WKWebView does not reliably execute `requestAnimationFrame`. `frontend_ready` therefore logs from the first committed React effect.
- Recents uses `/usr/bin/mdfind`. This avoids a Swift helper but does not reproduce Finder's private filtering rules.
- Quick Look uses `qlmanage -p` as a child process. Killing that child is the demo's toggle workaround. This is weaker than owning a native `QLPreviewPanel`.
- The local bundle has an ad hoc linker signature. It is not Developer ID signed or notarized.
