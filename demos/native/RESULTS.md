# AppKit demo results

Status: release build verified, static and real-launch smoke checks passed, sequential GUI benchmark still required.

## What is implemented

The release bundle covers the benchmark's visible vertical slice:

- hidden accessory application with one resident `920x640` panel;
- direct global `Command+Shift+F` registration, with an environment override for smoke runs;
- browser-style mouse-selectable and closable Tabs;
- Path Input with absolute paths, `~`, file selection, Recents, and Benchmark 10k;
- native `NSTableView` columns for name, kind, modified date, and size;
- asynchronous real directory loading;
- Spotlight Recents through `NSMetadataQuery`;
- Up, Down, `Ctrl+J/K`, Right, Left, Enter, Space, mouse, double click, and context click;
- system open, Copy Path, and `QLPreviewPanel`;
- all required NDJSON event names.

No destructive filesystem actions are present.

## Measurements collected

| Measurement | Result |
| --- | ---: |
| Clean release build, wall clock | 33.13 s |
| Clean release build, user CPU | 30.51 s |
| Clean release build, system CPU | 2.34 s |
| Cached release build | 0.14 to 0.15 s |
| Release executable | 326,864 bytes |
| Ad hoc signed `.app` disk use | 328 KiB |
| Third-party dependencies | 0 |
| Swift source files | 9 |
| Swift source lines | 983 |

The clean build used a fresh SwiftPM scratch directory. Module caches were already present because the compiler needs a writable cache inside the workspace. The raw timing output was:

```text
Build complete! (31.93s)
real 33.13
user 30.51
sys 2.34
```

`codesign --verify --deep --strict` passed. `plutil -lint` passed. The executable is arm64 Mach-O. The packaged executable's `--smoke` checks passed. A real hidden AppKit launch also recorded `backend_ready` with `shortcut_registered: true` while using temporary `Command+Shift+9`. `otool -L` reports only Apple frameworks and system Swift libraries.

Gatekeeper assessment is not meaningful for this local ad hoc build. `spctl` returned `internal error in Code Signing subsystem`; the bundle has no Developer ID team and is not notarized.

## Measurements still required

Do not infer a host winner from the build checks. This process has not been launched into the logged-in GUI session because the three host demos share one benchmark shortcut and are being built in parallel.

The sequential comparison must still collect:

- five cold launches to hidden-ready;
- thirty warm opening samples and field-focus failures;
- five directory loads and five Recents refreshes;
- 10,000-row keyboard and scroll behavior;
- idle and scrolling CPU and RSS;
- another Space, multiple monitors, fullscreen applications, both keyboard layouts, sleep and wake, and ten rapid toggles;
- actual Quick Look behavior and context-menu selection.

Use `scripts/summarize-log.sh` after the run and preserve `events.ndjson` with the comparison artifacts.

## Missing behavior and honest caveats

- `window_visible` records the next main-loop turn after `makeKeyAndOrderFront`, not the exact display-server presentation time. Its event includes `path_input_focused` and `window_key` so failed activations remain visible.
- The panel asks AppKit to move to the active Space and act as a full-screen auxiliary window. Those flags compile, but the required Spaces and fullscreen cases are unverified.
- Carbon normally keeps a registration across sleep and layout changes. The benchmark must test this machine rather than treat the API behavior as proof.
- A shortcut collision makes registration fail. The app logs `backend_ready` with `shortcut_registered: false` and stays alive, but it has no settings UI to repair the collision.
- Benchmark 10k Items have no real URL. Open, Copy Path, and Quick Look are intentionally disabled for them.
- Recents is Spotlight-derived and capped at 500 Items from the home scope. It will not exactly equal Finder Recents.
- iCloud placeholders depend on system file-provider behavior when opened. This demo does not show download state or progress.
- Tabs survive hiding, not app restart. There is no persistence layer.
- The app is arm64-only because the available machine and SwiftPM build produced one architecture.
- The ad hoc signature is enough for local measurement. Shipping needs signing, notarization, update delivery, and a decision about App Sandbox access.
- AppKit supplied every requested control, but the demo took 983 lines of Swift. Window and input behavior is direct. The UI construction and tab bookkeeping are much more verbose than React.

## Toolchain friction

The installed `swiftc` is Swift 6.3.2 build `swiftlang-6.3.2.1.108`. The default macOS 26.5 SDK's Swift interfaces identify build `swiftlang-6.3.2.1.2`, so a default SwiftPM build fails before compiling the package. The bundled macOS 15.4 SDK works and still supports the application's macOS 14 deployment target. Both scripts choose it explicitly.

The Command Line Tools installation also lacks a compatible test framework combination. XCTest is absent. The bundled Swift Testing framework requires `Swift.SendableMetatype`, which the compatible 15.4 SDK does not expose. `scripts/test.sh` therefore runs deterministic non-GUI checks from the release executable. Full Xcode would remove this limitation, but installing it is outside this benchmark.

## Launch defect found and fixed

The first sequential launch found an `NSInvalidArgumentException` before `backend_ready`. The Recents query used `%K != nil` in `NSPredicate`; the Spotlight predicate parser rejects `nil` on the right-hand side. The query now uses `kMDItemLastUsedDate >= cutoff`. Metadata without a last-used date fails that comparison naturally, so the intended 90-day Recents filter remains intact.

The same run reported an Auto Layout conflict between the 38-point Tab scroll view and a Tab stack constrained to both edges at 30 points. The stack now has a 30-point height and a vertical center constraint. It no longer claims the scroll view's full height.

The regression smoke now starts the actual application far enough to create `MainWindowController`, start `NSMetadataQuery`, install the event handler, and register a temporary global shortcut. It then checks the isolated NDJSON log for successful `backend_ready`. This would have caught the predicate exception.

## Current answer to the ticket

The source and release bundle prove that AppKit can express the required architecture without third-party code. They do not yet prove reliable focus, shortcut recovery, scrolling feel, Quick Look, or low resident overhead on the target desktop. The ticket should remain claimed until the sequential GUI run records those results.
