---
status: accepted
---

# Use Tauri for the macOS application

Visual Files will use Tauri v2 with a React UI in WKWebView and Rust for filesystem, Spotlight, and macOS integration. The packaged Tauri demo opened and showed its window noticeably faster than the Electron demo in Kirill's hands-on test, used less than half the visible memory in the automated sample, and kept the web UI model without Electron's 258 MiB bundle. Keyboard navigation and Quick Look still need a focused UX pass, but neither problem requires changing the host.

Electron is rejected because its slower launch and show behavior was noticeable in direct use, its Quick Look demo misbehaved, and its four-process sample used about 459 MiB. AppKit remains useful as a reference for native behavior, especially Quick Look, but the current product does not justify moving the whole UI into AppKit.

The first version targets macOS only. Cross-platform behavior is not a requirement. Tauri does not prevent a later port, but portability must not weaken macOS window behavior or integrations.

The Tauri implementation may use a small native macOS bridge where Rust and public Tauri APIs are insufficient. The first likely candidate is an owned `QLPreviewPanel` to replace the `qlmanage` demo workaround. Foundation-backed Spotlight, iCloud placeholder state, and precise window activation may also belong in that bridge.

Revisit this decision only if the production UX pass finds a WKWebView defect that cannot be fixed without changing the interaction model, or if a future product requires a large embedded TypeScript runtime or pixel-identical behavior across operating systems.
