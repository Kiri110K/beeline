# Choose the application host

Type: grilling
Status: resolved
Blocked by: none

## Question

After reviewing the migration research, raw benchmark results, missing behaviors, implementation friction, and Kirill's hands-on use of all three demos, which host should the production application use? Record why the rejected hosts lost, which native helpers the winner still needs, and what evidence would justify revisiting the decision.

## Comments

## Answer

Use Tauri v2 for the production macOS application. Kirill's direct comparison found the Tauri demo pleasant and the Electron demo noticeably slower to launch and show. Electron Quick Look also misbehaved. The automated samples support that perception: Tauri packaged at about 9.6 MiB and used about 190.8 MiB across its visible WebKit process set, while Electron packaged at about 258 MiB and used about 459.2 MiB across four processes.

Tauri's failed arrow-key behavior is not accepted as final behavior. It moves into the keyboard UX specification and implementation pass. The `qlmanage` Quick Look workaround also moves into the native-integration pass, likely through an owned `QLPreviewPanel` bridge. These are bounded implementation problems and did not make the whole Tauri host feel slow.

AppKit remains the behavioral reference for Quick Look and macOS accessibility, but a fully native UI does not currently justify its implementation cost. Cross-platform support is not a first-version constraint. If it becomes important later, Tauri gives us a path to it without making today's macOS app pay Electron's cost.

Revisit the decision only for a demonstrated, unfixable WKWebView limitation or a future requirement for a large embedded TypeScript runtime or identical rendering across operating systems. The durable record is `docs/adr/0001-use-tauri-for-the-macos-app.md`.
