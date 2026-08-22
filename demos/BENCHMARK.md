# Application host benchmark

## Purpose

Compare Tauri, Electron, and AppKit as hosts for the same shortcut-driven macOS file-browser experience. These are disposable demos, not three production implementations. The comparison must expose differences in launch behavior, focus, input, filesystem integration, rendering, memory, and development friction.

## Fairness rules

- Implement the same visible behavior and keyboard contract in every demo.
- Keep each app resident after first launch and hide its window when dismissed.
- Produce a release build. Development-server performance does not count.
- Run only one demo at a time because all demos register the same temporary global shortcut.
- Use `Command+Shift+F` for the benchmark. Production shortcut selection remains a separate product decision.
- Do not add destructive filesystem operations.
- Use the strongest normal approach for each host. The benchmark compares viable implementations, not artificially identical internals.
- The Tauri and Electron demos use React, the same virtualization strategy, identical generated data, and equivalent row markup and styles so the webview comparison is meaningful.
- Record missing or awkward behavior instead of silently replacing it with a different interaction.

## Shared experience

### Window lifecycle

- Launch as a background application with the main window hidden.
- `Command+Shift+F` shows a centered `920x640` window, activates the app, and focuses the Path Input.
- Pressing the shortcut while visible hides the window.
- `Escape` hides the window when no transient menu or Quick Look panel is open.
- Showing the window must work from another application, another Space, and a full-screen application when macOS permits it.
- The shortcut must recover after sleep/wake and work with both Russian and English keyboard layouts.
- Ten rapid shortcut presses must not create duplicate windows, lose focus permanently, or leave the app in an indeterminate visible state.
- Hiding the window must not terminate the app or discard Tabs.

### Layout

- A browser-style Tab strip sits at the top.
- Tabs can be created, selected, and closed with mouse controls.
- `Command+T` creates a Tab and focuses the Path Input.
- `Command+W` closes the active Tab. The final Tab resets instead of terminating the app.
- The Path Input appears below or inside the Tab strip and accepts an absolute path or `~` path.
- The main area is a virtualized list with columns or aligned fields for name, kind, modified date, and size when available.
- A lightweight status area exposes current Location, item count, and loading or error state.

### Data modes

- `Recents` uses Spotlight metadata and appears as the initial collection.
- A submitted directory path becomes the current Location and lists real entries asynchronously.
- A submitted file path selects that file in its parent Location.
- A `Benchmark 10k` Tab or command displays 10,000 deterministic synthetic Items without touching the filesystem.
- Directory enumeration must not block window display or keyboard input.

### Input

- Up/down and `Ctrl+J/K` move selection.
- Right Arrow enters a selected directory. On a file it performs the demo's documented open behavior.
- Left Arrow returns to the previous Location.
- Enter performs the primary action on the selected Item.
- Space opens Quick Look for a real file and closes it when pressed again.
- Single click selects. Double click performs the primary action. Context click opens a small action menu.
- Mouse wheel and trackpad scrolling must remain smooth in `Benchmark 10k`.

### Safe actions

- Open a file with the system default application.
- Copy the selected path.
- Quick Look the selected file.
- Do not implement Trash, rename, move, copy, or create operations in these demos.

## Instrumentation

Each app writes newline-delimited JSON events to its own user-data directory. Use a monotonic clock where available.

Required events:

- `process_start`
- `backend_ready`
- `frontend_ready`
- `shortcut_received`
- `show_requested`
- `window_visible`
- `path_submitted`
- `directory_loaded`, including duration and item count
- `recents_loaded`, including duration and item count
- `benchmark_10k_rendered`
- `quick_look_requested`
- `window_hidden`

The demo must include a short command or documented procedure that summarizes timing events from the log.

## Comparison runs

For each release build, collect:

- Five cold launches from process start to hidden-ready state.
- Thirty warm shortcut openings from `shortcut_received` to `window_visible` and focused Path Input.
- Five directory loads for the same local directory.
- Five Recents refreshes.
- `Benchmark 10k` first render and continuous keyboard navigation.
- Idle resident memory and CPU after two minutes with the window hidden.
- Visible-window memory and CPU while scrolling `Benchmark 10k`.
- Window activation and focus behavior across Spaces, multiple monitors, full-screen applications, Russian and English layouts, sleep/wake, and rapid repeated invocation.
- Build duration, release bundle size, dependency count, and any unsigned-build or permission friction.

Do not invent a winner from one timing. Preserve raw samples and report median, p95, slowest sample, and visible failures.

## Deliverables per demo

- Source under its assigned demo directory.
- A release `.app` or exact reproducible release-build command.
- `README.md` with build, run, and benchmark instructions.
- `RESULTS.md` containing measurements, missing behavior, framework-specific workarounds, and implementation notes.
- No development server left running.
