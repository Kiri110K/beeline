# Tauri demo results

Status: build complete, GUI comparison pending.

## What is implemented

The release build covers the shared layout, Tab lifecycle, Path Input, deterministic 10k mode, virtualized rows, keyboard navigation, mouse selection, double click, context menu, asynchronous real directory loads, Spotlight Recents, system file opening, path copying, Quick Look, hidden resident lifecycle, the configurable global shortcut, and all required NDJSON event names.

The Rust backend keeps directory enumeration and Spotlight metadata work away from the WKWebView thread. The frontend bundle does not call Node APIs.

## Verified automatically

Run on macOS 26.5.1, Apple Silicon, Bun 1.3.14, Rust 1.92.0:

- `bun run check`: passed.
- `bun run check:contract`: passed. It checks all required event names, hidden launch, 920 by 640 window size, default shortcut, and 10k mode.
- `cargo test --offline`: 3 passed. Home expansion, shortcut parsing, and real `/tmp` enumeration work.
- `bun run build:app`: passed.
- Release bundle: 9,816 KiB on disk.
- Release executable: 9,812 KiB, arm64 Mach-O.
- Incremental release build after source changes: 17.94 seconds wall time. This is not a clean build measurement.
- JavaScript install reported 74 packages. Cargo locked 439 packages, including build and platform transitive dependencies. The crate has 5 direct Rust dependencies including its build dependency.
- Signing: ad hoc linker signature. No Team ID, sealed resources, or notarization.

Full Xcode is not installed. Tauri still produced the `.app` with Command Line Tools.

## Hidden startup smoke

One valid release run used the temporary `Command+Shift+9` shortcut and stayed hidden:

| Event | Time from process start |
| --- | ---: |
| `backend_ready` | 2.20 ms |
| `frontend_ready` | 137.34 ms |
| `recents_loaded` | 239.61 ms |

The Recents query itself took 101.81 ms and returned 289 accessible items. The raw sample is in `raw/hidden-startup-smoke.ndjson`.

This is a smoke sample, not a benchmark result. It does not measure app activation, window presentation, input focus, or variance across cold launches.

An earlier run exposed a real WKWebView trap. The hidden webview loaded React and Recents, but a `requestAnimationFrame` callback never logged `frontend_ready`. Hidden WKWebViews throttle animation frames. The demo now records readiness from the committed React effect.

## Workarounds and risks

- Window activation relies on Tauri's accessory activation policy, `show`, and `set_focus`. This combination compiled, but cross-Space, full-screen, multi-monitor, and repeated-focus reliability remain unverified.
- Tauri's `macos-private-api` feature is required for visibility across workspaces. That API does not prove that macOS will place the panel above every full-screen application.
- `qlmanage -p` is an external-process Quick Look workaround. It does not provide the ownership and lifecycle control of `QLPreviewPanel`, and its kill-to-close behavior needs manual testing.
- Spotlight runs through `mdfind`, then Rust calls filesystem metadata for up to 500 returned paths. It is off the UI thread, but Finder's private Recents filters and true last-used ordering are absent.
- Open and Copy path use the macOS `open` and `pbcopy` executables. They worked at build level only. GUI invocation still needs testing.
- The transparent, undecorated WKWebView gives the demo its browser-like frame. Its scrolling and power cost must be measured, not guessed.
- No native Swift helper is present. iCloud placeholder state and native Quick Look panel ownership are outside this demo.

## Measurements still required

The benchmark question is not answered until someone runs the release bundle interactively and records:

- five cold launches;
- thirty warm shortcut openings with confirmed Path Input focus;
- five identical directory loads and five Recents refreshes;
- first 10k render plus sustained keyboard and trackpad use;
- hidden and visible CPU and memory;
- another Space, multiple monitors, full-screen apps, both keyboard layouts, sleep/wake, and ten rapid toggles;
- actual Open, Copy path, and Space Quick Look behavior.

These checks must run sequentially so the Tauri, Electron, and AppKit demos do not fight over shortcuts or foreground focus.

## Performance envelope run (2026-08-23, ticket #7)

Release bundle, Apple Silicon, macOS 26.5.1. Raw log: `raw/perf-ticket-run-20260823.ndjson`.

### Measured

- Cold entry: the first process start showed Recents at 543.6 ms (frontend ready 237.2 ms). Four immediate restarts with warm OS caches: frontend ready 126.7–129.6 ms, Recents shown 193.5–201.3 ms. A true post-reboot start was not measured.
- Warm entry: 30 shortcut invocations on the resident hidden process. `shortcut_received` to `window_visible`: median 2.4 ms, p95 8.7 ms, max 11.4 ms. Visual confirmation of Path Input focus per show was not automated.
- Recents (mdfind 90-day window plus metadata for up to 500 paths): 59–84 ms across 11 runs, 278–289 accessible items. The first query of the session took 305.6 ms.
- Directory enumeration, backend only (`read_dir` + per-item stat + sort): 8 items 0.05 ms; 63 items (`/Applications`) 0.2 ms; 50,000 synthetic items 299.4 ms first pass and 185.4 ms warm; a mounted 5,001-item APFS disk-image volume 16.5 ms; the iCloud Drive root (25 items) 2.2 ms with no downloads triggered.
- Idle resident, window hidden, after 2 minutes: `phys_footprint` 19 MB (peak 21 MB), RSS 90 MB, 0.0% CPU.

### Method

- A headless hook was added for this run: `VISUAL_FILES_HEADLESS_BENCH` takes semicolon-separated tasks (paths, or `recents`), runs them on the backend with the window hidden, logs the normal events with `origin: "headless"`, and exits. These numbers exclude IPC serialization and webview rendering.
- GUI keystroke automation (System Events) was aborted mid-run: global keystrokes landed in the active user's applications while they were working. Do not drive this demo through synthetic keystrokes while the machine is in use.

### Reading

- The backend is not the bottleneck. Even 50,000 stat-included enumerations fit in ~200–300 ms; ordinary directories are microseconds. The large-directory risk concentrates in the IPC payload and DOM rendering, which this run does not cover.
- Cached-first-paint Recents (#10) is confirmed viable: a background refresh costs ~70 ms.

### Still unmeasured (input to the performance contract, #12)

Frontend render cost of large listings, sustained-scroll CPU, `10k` first render, cross-Space and full-screen activation, per-show focus confirmation, icon and preview costs (absent from this demo), network volumes (none available), and a true post-reboot cold start.
