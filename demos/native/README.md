# AppKit host demo

This is the native comparison build for `../BENCHMARK.md`. It is a disposable Swift 6.3 and AppKit application, not a proposed production codebase.

The app launches without a Dock icon and keeps one hidden `NSPanel` resident. `Command+Shift+F` toggles the panel. The first show focuses and selects the Path Input.

## Build and run

The build needs macOS Command Line Tools. Full Xcode is not required.

```sh
cd /Users/kiri110k/lab/beeline/demos/native
./scripts/test.sh
./scripts/build-app.sh
open "dist/Visual Files Native.app"
```

`build-app.sh` makes an arm64 release bundle at `dist/Visual Files Native.app` and applies an ad hoc signature. A distributed build would still need a Developer ID signature and notarization.

The Command Line Tools installed on the benchmark Mac contain a mismatched default macOS 26.5 SDK. The scripts select the compatible bundled macOS 15.4 SDK. Override this on another machine with `VISUAL_FILES_SDKROOT=/path/to/MacOSX.sdk`.

`test.sh` makes a release build and runs two smoke checks. The first checks path expansion and deterministic 10,000-item data without AppKit startup. The second packages and starts the real `.app` through LaunchServices with a temporary `Command+Shift+9`. It constructs the window, starts the Spotlight Recents query, and requires a logged `backend_ready` event with successful shortcut registration. The installed Command Line Tools do not ship a usable XCTest or Swift Testing combination. This is recorded in `RESULTS.md`.

## Controls

- `Command+Shift+F` shows or hides the window.
- `Escape` closes Quick Look first, then hides the window.
- `Command+T` creates a Recents Tab and focuses the Path Input.
- `Command+W` closes the current Tab. The last Tab resets to Recents.
- `Command+R` refreshes the current Location.
- `Command+B` opens the deterministic Benchmark 10k data.
- Up and Down or `Ctrl+J/K` move the selected row.
- Right Arrow or Enter enters a directory or opens a file with its default application.
- Left Arrow returns to the previous Location in that Tab.
- Space toggles Quick Look for a real file.
- A single click selects. A double click performs the primary action.
- A context click offers Open, Quick Look, and Copy Path.

The Path Input accepts an absolute path, a path beginning with `~`, `Recents`, or `benchmark:10k`. Submitting a file path loads its parent and selects the file.

For a smoke launch that cannot collide with the comparison shortcut, run the executable directly with another physical key code. Key code 25 is the `9` key:

```sh
VISUAL_FILES_SHORTCUT_KEYCODE=25 \
VISUAL_FILES_SHORTCUT_MODIFIERS=command,shift \
"dist/Visual Files Native.app/Contents/MacOS/VisualFilesNative"
```

The release default remains `Command+Shift+F`. Carbon registers the physical F key, so the binding does not depend on the Russian or English layout.

The sequential GUI verifier can show and focus the prewarmed window without synthesizing a shortcut:

```sh
VISUAL_FILES_START_VISIBLE=1 \
VISUAL_FILES_SHORTCUT_KEYCODE=25 \
VISUAL_FILES_SHORTCUT_MODIFIERS=command,shift \
"dist/Visual Files Native.app/Contents/MacOS/VisualFilesNative"
```

This test-only hook leaves normal startup unchanged. It emits the standard `show_requested` and `window_visible` events with `"origin":"test_hook"`.

## Instrumentation

Events are appended to:

```text
~/Library/Application Support/VisualFilesAppKit/events.ndjson
```

Each line contains a wall-clock timestamp, a monotonic nanosecond value, the process ID, and event-specific fields. Summarize the raw file with:

```sh
./scripts/summarize-log.sh
```

The summary reports sample count, median, p95, and slowest value for hidden-ready, warm show, directory loads, and Recents. The `window_visible` event fires on the next main run-loop turn after AppKit orders the window and focuses the field. It is a consistent application-level proxy, not a Core Animation frame-presented timestamp.

For resident resource samples, find the PID and use the same command for every demo:

```sh
pgrep -x VisualFilesNative
ps -o pid,rss,%cpu,etime,command -p PID
```

Keep only this host running while collecting the comparison run. Quit it from Activity Monitor or with `kill PID` before starting another demo.

## Implementation notes

- `NSTableView` creates views only for visible rows and reuses them while scrolling. The demo does not need a third-party virtualization library.
- `NSMetadataQuery` searches the user's home scope for items used in the last 90 days and returns at most 500 rows. This resembles Finder Recents but cannot copy Finder's private filters exactly.
- Directory enumeration runs on a user-initiated background queue and publishes results on the main queue.
- The global shortcut uses Carbon `RegisterEventHotKey`. It does not require Accessibility permission.
- Quick Look uses `QLPreviewPanel`. File open and Copy Path use `NSWorkspace` and `NSPasteboard`.
- Tabs, their history, and loaded Items stay in memory while the window is hidden. They do not survive process termination.
