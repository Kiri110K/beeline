# Build the AppKit application-host demo

Type: prototype
Status: claimed
Assignee: /root/appkit_demo
Blocked by: none

## Question

Can an AppKit application satisfy every required behavior in `demos/BENCHMARK.md` with direct macOS integration, reliable window focus, a global shortcut, real filesystem navigation, Spotlight Recents, Quick Look, and low idle overhead without unacceptable implementation complexity? Build the disposable demo in `demos/native`, verify a release build, preserve raw measurements, and document every workaround or missing behavior.

## Comments

2026-08-21: `/root/appkit_demo` built the complete AppKit vertical slice in `demos/native` and produced the ad hoc signed release bundle at `demos/native/dist/Visual Files Native.app`. Release build, plist, code signature, architecture, static smoke, and packaged real-launch smoke pass. The bundle is 328 KiB with no third-party dependencies. See `demos/native/RESULTS.md` for implementation notes and raw build measurements.

The first sequential verifier found a launch-blocking Spotlight predicate exception and a Tab-strip constraint warning. The Recents predicate now compares `kMDItemLastUsedDate` directly with the 90-day cutoff; missing dates fail the comparison without using unsupported `nil`. The 30-point Tab stack is centered in its 38-point scroll view instead of being pinned to both vertical edges. A regression test now launches the packaged `.app` through LaunchServices, starts the Recents query, registers temporary `Command+Shift+9`, and requires `backend_ready` with `shortcut_registered: true`. Its output is clean and passed. `VISUAL_FILES_START_VISIBLE=1` is also available to the sequential verifier and marks normal show events with `origin: test_hook`.

The ticket remains claimed. The benchmark question cannot be resolved until one sequential GUI run measures shortcut-to-visible timing, field focus, Recents, directory loads, 10k scrolling, memory, CPU, Spaces, fullscreen, keyboard layouts, sleep/wake, repeated toggles, and Quick Look. Running that now would collide with the Tauri and Electron demos using the same shortcut.

A sequential visible-UI pass now confirms native Quick Look, individually accessible rows and menu actions, real-directory navigation, Tabs, and 10k rendering. It also found three remaining product defects: hidden entries such as `.scratch` are omitted, rapid keyboard selection can outrun viewport scrolling by one event, and native Recents returned fewer results more slowly than both web hosts. The one-shot visible RSS was about 138.5 MiB. See `demos/GUI-VERIFICATION.md`. Physical shortcut, Spaces, layouts, sleep/wake, repeated samples, and subjective scrolling still keep this ticket claimed.
