# Sequential GUI verification

Run: 2026-08-21 on Kirill's Mac. Each packaged release was launched alone with its test-only visible-start hook, exercised through the real UI, sampled once for process memory, and stopped before the next host was started. The default release behavior remains hidden until the global shortcut is pressed.

This pass is evidence for deciding what to test by hand. It is not the final host decision: the timings and memory figures below are single samples, and macOS does not allow the automated verifier to reproduce a physical global shortcut across every Space, layout, and sleep cycle.

## Result

All three demos rendered the requested browser shape, loaded real filesystem data without Finder, navigated folders, created and closed Tabs, rendered a 10,000-row synthetic directory, and invoked Quick Look. None is yet a clean winner.

| | Tauri | Electron | AppKit |
| --- | ---: | ---: | ---: |
| Packaged size | 9.6 MiB | 258 MiB | 328 KiB |
| Process start to visible | 186 ms | 437 ms | 758 ms |
| Show request to visible | 32 ms | 52 ms | 652 ms |
| Initial Recents | 91 ms / 290 items | 159 ms / 291 items | 623 ms / 212 items |
| Repository directory load | 0.57 ms / 3 items | 13.11 ms / 3 items | 0.85 ms / 2 items |
| One-shot visible RSS | 190.8 MiB | 459.2 MiB | 138.5 MiB |
| Quick Look | external `qlmanage` | external `qlmanage` | native `QLPreviewPanel` |

The AppKit visible time is dominated by waiting for its first Spotlight query. It is not evidence that AppKit window activation itself needs 652 ms. Likewise, the RSS figures are neither two-minute idle samples nor sustained-scroll samples.

## What felt and behaved different

### Tauri

- The visual shell and 10k list stayed responsive, and its total WebKit footprint sat between AppKit and Electron.
- Initial keyboard focus was exposed as the WKWebView document, not the Path Input. A click in the list was needed before Up, Down, and `Ctrl+J/K` worked after path submission.
- Space launched and closed the Quick Look helper, but the verifier could not inspect or capture that separate process.
- Escape while the context menu was open hid the entire app instead of dismissing only the menu.
- The 10k list visibly worked, but this run did not emit `benchmark_10k_rendered`; the instrumentation needs fixing.

### Electron

- It had the cleanest observed focus behavior: Path Input was focused at launch, and Down transferred directly into list navigation.
- The 10k list kept selection and viewport in sync during the automated burst.
- Space launched and closed the Quick Look helper, but focus returned to the HTML document afterward and the helper was not inspectable.
- A persistent yellow warning glyph appeared in the status area without an accessible label, so its cause is still unknown.
- It was by far the largest bundle and used roughly 2.4 times Tauri's visible RSS in this one sample.

### AppKit

- Native Quick Look was fully visible, addressable, and closable while preserving selection. Native rows and context-menu items were also exposed individually.
- Path Input had focus at launch, but a list click was still needed before keyboard navigation after path submission.
- Its directory enumeration omitted `.scratch`, unlike both web hosts. That behavior must be made consistent before judging filesystem correctness.
- During forty rapid Down presses, the selection index advanced while the viewport lagged until one additional keypress forced the selected row into view.
- Recents returned materially fewer rows and was slower than the `mdfind` implementations. The query and first-window ordering need work.
- The first verifier exposed a crashing Spotlight predicate and a contradictory Tab-strip constraint. Both are fixed, packaged, and covered by the launch smoke test.

## What still needs Kirill at the keyboard

- Press the real `Command+Shift+F` shortcut from ordinary apps, full-screen apps, and another Space.
- Try 30 warm opens and 10 rapid show/hide toggles. Confirm that the Path Input is ready immediately every time.
- Repeat on Russian and English layouts, then after sleep and wake.
- Flick-scroll Benchmark 10k with the trackpad and notice input lag, blank rows, fan/energy behavior, and whether selection follows the viewport.
- Open and close Quick Look in Tauri and Electron and judge whether the external helper feels like part of the app.
- Try Open and Copy Path on disposable files. These were not invoked automatically because they affect external applications or the clipboard.
- Collect the benchmark's five cold starts, repeated directory and Recents samples, two-minute idle RSS/CPU, and sustained-scroll RSS/CPU.

## Decision after hands-on use

Kirill tested the real Tauri and Electron builds through `Command+Shift+F` on 2026-08-22. Tauri felt pleasant. Its arrow navigation appeared not to work, which remains a required UX fix. Electron was noticeably slower to launch and show, and its Quick Look behavior failed during the test.

Tauri is the production host. Electron and AppKit remain disposable references. See [`../docs/adr/0001-use-tauri-for-the-macos-app.md`](../docs/adr/0001-use-tauri-for-the-macos-app.md).

## Artifacts

- Tauri: [`tauri/src-tauri/target/release/bundle/macos/Visual Files Tauri Benchmark.app`](tauri/src-tauri/target/release/bundle/macos/Visual%20Files%20Tauri%20Benchmark.app)
- Electron: [`electron/release/mac-arm64/Visual Files Electron Demo.app`](electron/release/mac-arm64/Visual%20Files%20Electron%20Demo.app)
- AppKit: [`native/dist/Visual Files Native.app`](native/dist/Visual%20Files%20Native.app)
- Shared procedure: [`BENCHMARK.md`](BENCHMARK.md)

All demo and Quick Look processes were stopped after verification.
