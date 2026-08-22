# Build the Electron application-host demo

Type: prototype
Status: claimed
Assignee: /root/electron_demo
Blocked by: none

## Question

Can an Electron application satisfy every required behavior in `demos/BENCHMARK.md` with a responsive web UI, reliable macOS window focus, a global shortcut, real filesystem navigation, Spotlight Recents, Quick Look, and acceptable idle overhead? Build the disposable demo in `demos/electron`, verify a release build, preserve raw measurements, and document every workaround or missing behavior.

## Comments

The source, arm64 release build, raw hidden-release smoke log, and automated checks are complete in `demos/electron`. The ticket remains claimed because macOS denied synthetic keystrokes and the shared benchmark requires sequential manual shortcut, focus, Spaces, full-screen, sleep/wake, keyboard-layout, scrolling, and two-minute resource measurements. See `demos/electron/RESULTS.md` for exact results and remaining work.

A sequential visible-UI pass confirmed the cleanest launch focus and keyboard handoff of the three hosts, working real-directory navigation, Tabs, context menu, and a synchronized 10k list. The external `qlmanage` preview remained inaccessible, focus returned to the document after preview, and an unlabeled warning glyph was visible. The one-shot four-process total was about 459.2 MiB. See `demos/GUI-VERIFICATION.md`. The physical and repeated portions of the benchmark still keep this ticket claimed.
