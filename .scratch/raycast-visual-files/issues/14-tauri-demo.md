# Build the Tauri application-host demo

Type: prototype
Status: claimed
Assignee: /root/tauri_demo
Blocked by: none

## Question

Can a Tauri v2 application satisfy every required behavior in `demos/BENCHMARK.md` with a responsive web UI, reliable macOS window focus, a global shortcut, real filesystem navigation, Spotlight Recents, Quick Look, and low idle overhead? Build the disposable demo in `demos/tauri`, verify a release build, preserve raw measurements, and document every workaround or missing behavior.

## Comments

- Claimed by `/root/tauri_demo` on 2026-08-21. Building the disposable Tauri v2 benchmark under `demos/tauri`.
- Implementation complete on 2026-08-21. Release bundle: `demos/tauri/src-tauri/target/release/bundle/macos/Visual Files Tauri Benchmark.app`. TypeScript, the static benchmark contract, three Rust smoke tests, release packaging, and a hidden-startup smoke run pass. `demos/tauri/RESULTS.md` records the 137.34 ms frontend-ready sample, 101.81 ms Spotlight query, bundle size, workarounds, and raw log path.
- Status remains claimed. The full question still requires the shared sequential GUI run: 30 warm shortcut openings with confirmed focus, Spaces/full-screen/multiple monitors, Russian and English layouts, sleep/wake, rapid toggles, 10k scrolling, Quick Look, and CPU/memory. A bounded computer-use attempt was stopped before app launch because the verifier recursively delegated the same check instead of operating the GUI. No benchmark process was left running.
- A later sequential visible-UI pass exercised real Recents and directories, keyboard navigation, Tabs, context menu, Quick Look launch, and the 10k list. Core browsing worked. Remaining defects are initial focus exposure, Escape hiding the app from an open context menu, an inaccessible external Quick Look helper, and missing 10k instrumentation. The one-shot visible total was about 190.8 MiB across the app and its contemporaneous WebKit processes. See `demos/GUI-VERIFICATION.md`. Physical shortcut, Spaces, layouts, sleep/wake, repeated samples, and subjective scrolling still keep this ticket claimed.
