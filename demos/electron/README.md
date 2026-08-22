# Electron application-host demo

This disposable Electron app implements the shared behavior in `../BENCHMARK.md`. It stays resident with a hidden window and toggles from `Command+Shift+F`.

## Build

```sh
bun install
CSC_IDENTITY_AUTO_DISCOVERY=false bun run bundle
```

The unsigned arm64 release appears at `release/mac-arm64/Visual Files Electron Demo.app`.

## Run

Quit the other host demos first because the benchmark shortcut must have one owner.

```sh
open "release/mac-arm64/Visual Files Electron Demo.app"
```

For smoke testing with a non-conflicting shortcut, start the executable directly:

```sh
VISUAL_FILES_SHORTCUT="CommandOrControl+Shift+E" "release/mac-arm64/Visual Files Electron Demo.app/Contents/MacOS/Visual Files Electron Demo"
```

If the parent shell defines `ELECTRON_RUN_AS_NODE=1`, unset it for a real app launch:

```sh
env -u ELECTRON_RUN_AS_NODE VISUAL_FILES_SHORTCUT="CommandOrControl+Shift+E" "release/mac-arm64/Visual Files Electron Demo.app/Contents/MacOS/Visual Files Electron Demo"
```

GUI automation can opt into one visible launch after the prewarmed renderer reports ready. This hook is disabled unless its value is exactly `1`:

```sh
env -u ELECTRON_RUN_AS_NODE VISUAL_FILES_START_VISIBLE=1 VISUAL_FILES_SHORTCUT="CommandOrControl+Shift+E" "release/mac-arm64/Visual Files Electron Demo.app/Contents/MacOS/Visual Files Electron Demo"
```

The hook calls the normal show and Path Input focus path. Its `show_requested` and `window_visible` events use `"origin":"test-hook"`. It does not test global shortcut delivery, so the benchmark still requires real `Command+Shift+F` presses.

The app has no Dock icon. Use the shortcut to show or hide it. `Escape` hides the window without terminating the process. Quit it before testing another host:

```sh
pkill -x "Visual Files Electron Demo"
```

## Controls

- `Command+T` creates a Recents tab and focuses Path Input.
- `Command+W` closes the active tab. The final tab resets to Recents.
- Paste an absolute path or `~/path`, then press Enter.
- Up/down or `Ctrl+J/K` changes selection.
- Right Arrow or Enter opens a file or enters a directory.
- Left Arrow returns to the previous directory.
- Space toggles Quick Look for a real file after the Path Input loses focus.
- Single click selects, double click opens, and context click opens safe actions.
- `Benchmark 10k` opens the shared deterministic synthetic dataset.

## Instrumentation

Events are appended to:

```text
~/Library/Application Support/Visual Files Electron Demo/events.ndjson
```

Summarize the samples with:

```sh
bun run summarize
```

Pass an alternate NDJSON path as the final argument when preserving a comparison run. The summary reports median, p95, slowest sample, focus failures, and renderer failures.

The checked-in `measurements/release-smoke.ndjson` is the first hidden release smoke sample. It does not count as a comparison run.

Non-GUI filesystem and Spotlight checks:

```sh
bun run smoke
```

Follow every comparison run in `../BENCHMARK.md`. The release build still needs the sequential GUI measurements listed in `RESULTS.md`.
