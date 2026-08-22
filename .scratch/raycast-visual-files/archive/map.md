# Wayfinder Map: Visual Files

Status: charted
Label: wayfinder:map

## Destination

A decision-complete, implementation-ready specification for a personal, shortcut-driven macOS file browser that minimizes the need to use Finder for everyday file access. Another session must be able to implement the first version from the specification without inventing product behavior, safety rules, integration semantics, application-host behavior, or performance targets.

The first version must make opening a copied path, using system Recents, visually browsing folders, previewing files, copying paths, deleting items, and opening files or folders in the appropriate application feel immediate. It must support keyboard-first use without sacrificing complete mouse operation.

## Notes

- Primary user: Kirill. Optimize for his workflow before general distribution or marketplace expectations.
- Product surface: a dedicated Tauri macOS application opened by a single configurable global shortcut, currently imagined as `Super+F`.
- Tauri v2 is the chosen application host. React owns the UI, Rust owns filesystem and system work, and small native macOS bridges are allowed where Tauri does not expose the right behavior.
- Primary presentation: a dense list, not a thumbnail grid.
- Mental model: a browser-like row of Temporary Tabs and Pinned Tabs above the current list.
- Speed is a product constraint, not a later optimization. Cold entry, warm entry, navigation, and large-directory behavior require explicit budgets.
- The Finder Recents screenshot supplied during discovery is the visual anchor for treating Recents as a first-class starting collection.
- The canonical domain language is in `../../CONTEXT.md`.

## Decisions so far

- This effort produces specifications, decision records, and disposable architecture demos; it does not implement the production application.
- Raycast is no longer assumed to host the product. It may remain an optional launcher integration later.
- A warm application return restores the complete active browsing context except transient overlay layers. After five minutes in the background, Pinned Excursions reset to their Anchors on the next invocation; Temporary Tabs remain until their independently configured lifetime expires.
- A new Temporary Tab starts at the Default Entry Point and activates the Navigation Input so a copied path or fuzzy query can be entered immediately. The default is Recents and Settings may select another Location.
- Recents in version 1 means system-derived Recents. From version 1 the application also keeps a Visit Journal — entered Locations and file opens — solely as ranking input for Search, not as a user-facing collection. This supersedes the earlier exclusion of an extension-owned visit history, which was recorded before search became the primary entry mechanism. [Context](issues/09-recents-and-entry-points.md)
- Up/down navigation is mandatory. `Ctrl+J/K` may mirror it if this does not create a separate interaction model.
- The primary action on a directory is to enter it. The primary action on a file is to open it with the configured application behavior.
- The application must expose Quick Look, copy-path, deletion, and useful secondary open actions without requiring a different shortcut for every operation.
- Full mouse support is required alongside keyboard navigation.
- All content in the Accessible Filesystem is conceptually in scope, including iCloud Drive and mounted volumes; actual platform constraints must be researched and made explicit.
- Raycast research established why it is not the assumed host: it cannot provide a true top Tab strip, raw key events, custom row mouse handlers, or an inspectable navigation stack. [Context](issues/01-raycast-v2-extension-surface.md)
- Recents research: version 1 can use cached asynchronous Spotlight results via `mdfind` plus batched metadata reads, but exact Finder parity is impossible because Finder relies on private scopes and filters. [Context](issues/02-system-recents.md)
- Filesystem research: the extension can browse mounted paths accessible to the Raycast process; TCC grants are app-wide, and polished iCloud placeholder state or explicit download control requires a native Foundation bridge beyond the public Raycast API. [Context](issues/03-filesystem-icloud-access.md)
- Integration research: macOS provides the required Quick Look, clipboard, Trash, default-open, and Open With behavior, though each application host needs its own adapter. Ghostty and Zed accept directories through Launch Services, while T3 Code currently exposes no stable directory-open contract. [Context](issues/04-file-and-app-integrations.md)
- The Raycast shell prototype was superseded by an application-host comparison after the product moved to its own window. [Context](issues/05-browser-shell-prototype.md)
- Migration research: OpenCode moved from current Tauri v2 because its complex UI behaved worse in WebKit and its TypeScript server fit Electron's embedded Node runtime better. Only the WebKit risk transfers strongly to this macOS-only file browser. ChatWise's migration is confirmed, but its reasons are not public. [Context](issues/13-tauri-to-electron-migrations.md)
- The first sequential GUI pass found no decisive host winner. Electron currently has the cleanest focus and keyboard behavior; AppKit has the strongest Quick Look and accessibility integration; Tauri is the middle ground on bundle size, memory, and web-UI iteration. Physical shortcut and feel testing remains required. [Comparison](../../demos/GUI-VERIFICATION.md)
- Kirill's hands-on comparison selected Tauri. It felt pleasant and opened noticeably faster than Electron. Electron's slower entry and broken Quick Look outweighed its cleaner prototype keyboard focus. The keyboard and Quick Look defects in Tauri are assigned to the production UX and native-integration passes rather than treated as host blockers. [ADR](../../docs/adr/0001-use-tauri-for-the-macos-app.md)
- [Decide the navigation and action model](issues/07-navigation-action-model.md) — Browse and Search are explicit logical states; Search Results Reveal into the browser; selection, actions, Preview Panel, Quick Look, Escape, and post-action behavior now have one keyboard and mouse contract.
- [Prototype Navigation Input, Search Results, and Preview Panel](issues/19-prototype-navigation-search-preview.md) — Kirill selected the browser-table composition: browser-like Tabs, a full-width Navigation Input, dense table, and fixed right-side Preview Panel. The other compositions remain disposable explorations.
- [Decide Tab lifecycle](issues/08-tab-lifecycle.md) — Pinned Tabs are protected Anchors with warm Pinned Excursions; Temporary Tabs are disposable, reusable browsing contexts with a configurable three-hour default lifetime. Ordering, restoration, Search routing, shortcuts, closing, overflow, and the single-window lifecycle now have one contract.
- [Decide Recents and entry points](issues/09-recents-and-entry-points.md) — There is no sidebar: Search is the primary entry mechanism, Known Places and the Alias Dictionary are ranking boosts, and work folders stay Pinned Tabs. The Recents Tab is system-derived, files-only, newest-first, progressively loaded, with explicit degraded states. Hidden entries are always listed and search-penalized rather than excluded.

## Not yet specified

- The ranking-search design: engine choice, hidden-location scope, junk protection, the ranking formula, Visit Journal mechanics, and search latency budgets. [Open ticket](issues/20-ranking-search.md)
- Product behavior for denied application permissions, disconnected or read-only volumes, and indeterminate iCloud placeholder state.
- The complete file-operation set and its safety contract: Trash versus permanent deletion, confirmations, undo/recovery, failures, stale entries, and permission errors.
- How application routing works for files and directories, including default app, choose-app, terminal, IDE, and unavailable-application states.
- Measurable performance budgets and the degradation strategy for large directories, metadata, icons, previews, network volumes, and cloud-backed files.
- Navigation Input behavior for relative, mistyped, missing, and recently deleted paths, including how errors appear without exposing backend exception text.

## Out of scope

- Production implementation during this planning and architecture-comparison effort.
- An extension-owned Recents collection in version 1. The Visit Journal is ranking input for Search, not a Recents replacement.
- Replacing non-file-management Finder features such as desktop management or device administration.
- Windows or Linux support.
- Multiple windows and detached Tabs. Multi-monitor behavior is also outside version 1; the single window is centered by default and may later gain per-display placement memory.
- Marketplace positioning, onboarding for a general audience, monetization, or team administration in the first personal version.
