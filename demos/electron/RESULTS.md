# Electron results

## Build and automated checks

- `bun run typecheck`: passed for the renderer, main process, and preload.
- `bun test`: 5 tests passed, 0 failed. The tests cover deterministic 10,000-row data, selection bounds, size formatting, and the exact opt-in contract for the visible launch hook.
- `bun run smoke`: async directory enumeration and file-in-parent selection passed. The sandbox had Spotlight disabled and returned zero Recents; the normal release process returned 289.
- `CSC_IDENTITY_AUTO_DISCOVERY=false bun run bundle`: passed. A cached unsigned arm64 build took 5.9 seconds. The first run needed network access to download Electron.
- Release artifact: `release/mac-arm64/Visual Files Electron Demo.app`, arm64 Mach-O, 264,412 KiB on disk.
- Frontend payload: 225.95 kB JavaScript and 3.87 kB CSS before gzip.
- `bun pm ls --all` reports 410 dependency-tree entries including build tooling. `node_modules` has 258 top-level package directories.

## Hidden release smoke

The packaged app launched with temporary shortcut `Command+Shift+E`, registered it successfully, stayed resident, loaded its renderer, and queried 289 Spotlight results. Raw events are in `measurements/release-smoke.ndjson`.

- Backend ready: 214.04 ms after process start.
- Frontend ready: 422.20 ms after process start.
- Initial Recents: 95.56 ms for 289 items.
- One immediate hidden-process snapshot reported 0.0% CPU and about 366.5 MiB RSS across the main, renderer, GPU, and network processes. This was not the required two-minute idle measurement.
- No renderer failure appeared in the event log.

The harness inherited `ELECTRON_RUN_AS_NODE=1`; a real launch requires unsetting it. This is specific to the automation environment, not normal macOS launches.

## Visible test-hook smoke

The rebuilt packaged app launched with `VISUAL_FILES_START_VISIBLE=1` and temporary shortcut `Command+Shift+E`. After `frontend_ready`, it emitted `show_requested` and `window_visible` with `"origin":"test-hook"`. Path Input reported focused.

- Show request to visible and focused: 67.22 ms.
- `window_visible.focused`: `true`.
- Raw events: `measurements/start-visible-smoke.ndjson`.

A second packaged launch omitted the hook. It emitted `startVisibleForTest:false` and no show events, which confirms that the default release still starts hidden. Its raw events are in `measurements/default-hidden-smoke.ndjson`.

## Implemented behavior

- One resident, initially hidden `BrowserWindow` with a configurable global shortcut.
- A 920 by 640 frameless macOS panel on all Spaces, including full-screen Spaces where macOS permits it.
- Mouse tabs, `Command+T`, `Command+W`, focused Path Input, history, async directories, Spotlight Recents, and the deterministic 10,000-row dataset.
- The shared keyboard contract, safe file actions, context menu, and raw NDJSON instrumentation.
- Context isolation, sandboxed renderer, disabled Node integration, and a narrow preload bridge.
- An opt-in `VISUAL_FILES_START_VISIBLE=1` hook shows the prewarmed window through the normal focus path for GUI verification. The default remains hidden and shortcut-driven.

## Workarounds and missing behavior

- Electron has no Quick Look API. The demo launches `/usr/bin/qlmanage -p` and terminates that child on the second Space. This can briefly show `qlmanage` as a separate process and does not provide the control of `QLPreviewPanel`.
- Electron has no first-class non-activating `NSPanel` API. The demo uses `BrowserWindow` with `type: "panel"`, `setVisibleOnAllWorkspaces`, `app.focus({ steal: true })`, and explicit renderer focus. Spaces and full-screen behavior require manual verification.
- Recents come from `mdfind` and approximate Finder Recents. Finder's private filters and ranking are unavailable.
- Spotlight is disabled in the build sandbox used for automated verification, so the smoke check returned zero Recents with `Spotlight server is disabled.` The release app exposes that warning in the status area. Recents count and timing still need a run in the user's normal login environment.
- Directory metadata uses asynchronous `fs.stat` calls with 32 workers. A huge remote or cloud directory can finish slowly, though it does not block the renderer.
- The red, yellow, and green circles are visual only because the benchmark window is frameless. The benchmark does not require standard window buttons.
- The global shortcut uses Electron's accelerator registration. Russian and English layouts, recovery after sleep, and shortcut ownership conflicts need manual testing on the target Mac.
- macOS denied automation permission to the attempted `System Events` keystroke, so the smoke run verified shortcut registration but could not produce a real `shortcut_received` or `window_visible` sample. The comparison run must invoke the shortcut by hand.
- The app is unsigned and not notarized. The release bundle is for local benchmarking only.

## Sequential GUI measurements still required

No result is claimed until one demo owns the benchmark shortcut and the following checks run on the same machine as the Tauri and AppKit builds:

- Five cold starts to hidden-ready.
- Thirty warm shortcut openings with focus checks.
- Five loads of the shared local directory and five Recents refreshes.
- Continuous keyboard and trackpad use in Benchmark 10k.
- Hidden idle CPU and memory after two minutes, plus visible scrolling CPU and memory.
- Multiple monitors, Spaces, a full-screen app, both keyboard layouts, sleep/wake, and ten rapid shortcut presses.
- Release bundle size, dependency count, and build duration comparison.

Raw samples belong in `events.ndjson` or a copied run-specific NDJSON file. Run `bun run summarize` to calculate median, p95, and slowest values.
