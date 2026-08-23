# Beeline — implementation-ready specification, version 1

Beeline is a personal, shortcut-driven macOS file browser that replaces Finder for everyday file access: open a copied path, use Recents, browse and preview, copy paths, delete, and open files or directories in the right application — all keyboard-first with complete mouse support. Primary user: Kirill; the reference machine is his Apple Silicon MacBook.

This document synthesizes every resolved decision. Sources of record: the glossary (`/CONTEXT.md` — all capitalized terms below are defined there), ADR-0001 (Tauri host) and ADR-0002 (React UI) in `/docs/adr/`, and the resolution comments of the GitHub issues cited as `#N`. If this document and an issue resolution disagree, this document wins; report the discrepancy.

An implementer must not invent product behavior. Anything genuinely unspecified here is either listed in "Deliberately unspecified" (production-UX freedom) or is a bug in this spec.

## 1. Architecture

- Host: Tauri v2. React (current version, React Compiler enabled) in the WKWebView owns the UI; Rust owns the filesystem, the Name Index, Spotlight queries, the Visit Journal, and system integration (ADR-0001, ADR-0002).
- Small native macOS bridges are allowed where public Tauri APIs fall short. Expected candidates: an owned `QLPreviewPanel` for Quick Look, precise window activation, and (post-v1) iCloud placeholder state (ADR-0001).
- Bundle id `com.kiri110k.beeline`. Beeline is the version-1 working name; the final name is decided before any public release (the bare name is crowded publicly — App Store, the Apache Hive `beeline` CLI, the RU telecom brand). UI language: English. Localization is a desired post-v1 addition, so do not hard-code strings in components.
- The visual style of the selected mockup (`prototypes/navigation-search-preview/design-a-browser.html`) is the compositional reference, not final styling; a style rework is planned as the first post-v1 update.

## 2. Window and lifecycle

- Exactly one application window, ever. No detached Tabs, no multi-window (map, #9).
- The app is a Login Item: it starts hidden at login and prewarms — process, persisted Name Index, Recents cache — within 2 s (#12). A user-visible cold start exists only on first install or after a crash and must reach a usable window in ≤800 ms (#12).
- The global shortcut is configurable; the default is `Ctrl+Opt+Cmd+F`. It behaves as a toggle: shows and focuses a hidden window, focuses a visible window when another app is active, hides Beeline when Beeline is already active (#9).
- The window opens centered; it is freely draggable, and center guide lines give a snap target back to center. Multi-monitor placement memory is out of scope for v1 (#9).
- A warm return restores the active Tab, Location, Focused Item, Selected Items, scroll, navigation history, and retained Navigation Input text. Quick Look, the Action Menu, and active Search Results never survive hiding (#9).
- After five continuous background minutes (sleep counts), every Pinned Excursion resets to its Anchor on the next invocation; nothing resets while the window is visible (#9).

## 3. Layout (design A, #20)

Top to bottom: the Tab strip (Pinned group left, Temporary group right); the full-width Navigation Input; the dense file table (name, kind, modified, size); the fixed-width right-side Preview Panel (baseline 300 px, toggleable in Settings); and the Status Strip, the single small persistent feedback zone (#11). Exact spacing, typography, and the Status Strip's placement are production UX.

## 4. Tabs (#9)

Implement the full ticket-#9 contract. Summary of the load-bearing rules:

- Every Tab owns its Location, back/forward history, Focused Item, Selected Items, scroll, and retained inactive Search Query. A Temporary Tab's title follows its Location; a Pinned Tab's title is its Anchor name (renameable) and does not change during a Pinned Excursion.
- A new Temporary Tab starts at the Default Entry Point (Recents unless Settings picks another Location) with the Navigation Input active. Default Temporary lifetime: 3 hours since last activation; Settings offers 30 m / 1 h / 3 h / 6 h / 12 h / 24 h / Never. Expired Tabs are removed only in the background or on the next invocation — never while visible. If no Tab remains, a clean Temporary Tab is created; a tabless window does not exist.
- Pinned Tabs are protected Anchors: navigation from one creates a Pinned Excursion that survives Tab switching; clicking the active Pinned Tab returns to its Anchor. Two Pinned Tabs cannot share an Anchor. Pinning a Temporary Tab anchors its current Location; unpinning converts to Temporary in place. Pinned Anchor, custom name, and order survive restart (persist promptly); Excursions, selection, and history do not. Temporary Tabs do not survive restart.
- Search routing: reveal below the most specific matching Anchor activates that Pinned Tab (a file → its containing Location with the file focused; the Anchor itself → the Anchor). A directory below an Anchor, or any result outside all Anchors when Search began in a Pinned Tab, opens or reuses a Temporary Tab. Search begun in a Temporary Tab reuses it. Automatic routing reuses a Temporary Tab already at the target Location; explicit `Open in New Tab` always creates one. Ordinary directory entry reuses the current Tab.
- Ordering and input: user-ordered groups; dragging across the group boundary pins or unpins immediately and reversibly. `Cmd+1…8` by visible position, `Cmd+9` last, `Ctrl+Tab`/`Ctrl+Shift+Tab` cycle. `Cmd+W` closes a Temporary Tab; on a Pinned Tab it hides the window. A Pinned Tab has no incidental close control; removal is the explicit `Remove Pinned Tab` menu action. Temporary Tab menu: Pin, Close, Copy Location. Pinned menu: Rename, Unpin, Remove Pinned Tab, Copy Location. New Temporary Tabs insert beside their originator (after the pinned group); closing returns activation to the originator, else nearest left.
- Overflow: Tabs shrink to ~120–140 pt, then the strip scrolls horizontally with an all-Tabs searchable control. Exact feel is production UX.

## 5. Navigation, selection, actions (#8)

Implement the full ticket-#8 contract. Load-bearing rules:

- Two logical states independent of DOM focus. Search Mode: Search Results float over the Browse list; the first result is auto-focused; Up/Down move, Left/Right edit the query, Enter/Right/click performs Reveal. Browse Mode: Up/Down move through Items, Left goes back in history, Right or Enter runs the Focused Item's primary action; `Cmd+[`/`Cmd+]` are back/forward always; history restores selection and scroll.
- `Ctrl+J/K` mirror Down/Up in Browse, Search Results, and Quick Look, but are not intercepted while the Navigation Input is active. `Cmd+K` opens the Action Menu. Plain letters do nothing in Browse Mode in v1 (`h/j/k/l` reserved for a later Vim option; type-to-filter and query history are out).
- Escape closes the topmost layer in order: Quick Look → Action Menu → Search Results → Selected Items → window. Closing Search Results restores the prior Browse selection; the query stays visible, inactive, unselected. `Cmd+L`, clicking the input, or the physical Slash key (both RU and EN layouts) reactivates Search Mode and selects the retained query.
- Selection: one Focused Item; Shift+arrows / Shift+Click extend a range, Cmd+Click toggles; plain Up/Down collapse selection to the focused row. Enter/Right act on the Focused Item only; batch Open is an explicit Action Menu command. One click selects, double click runs the primary action, context click opens the Action Menu. In Search Results a single click Reveals.
- Primary actions are configured per kind in Settings: directory default Enter Location, file default Open with Default App; either may be Show Action Menu instead.
- The Action Menu (`Cmd+K`, `…` control, context click — one menu) shows applicable file actions with Selected Items, application actions (New Tab, Paste Path, Refresh, Settings, Quit) without. Quick Look and the Action Menu never coexist.
- Deletion shortcut `Cmd+Delete` (configurable later); plain Delete never deletes files.

## 6. Search (#21, #19, #10)

- All Navigation Input text is a Search Query through one ranker; there are no input modes. Path-shaped input is a ranking signal: an existing absolute path ranks its target first, deterministically; matches under a typed path prefix get scope priority and no hidden penalty. `/`, `~`, `./`, `../` prefixes and mid-string slashes all just shape ranking; slashes in any query match path segments (#19 amendment).
- Engine: the application's own Name Index — names and paths only, no content search in v1. Scope: the home directory plus mounted local volumes. Spotlight's only role is sourcing the Recents collection; any future Spotlight use is a new ticket (#21). Evidence constraint: Spotlight cannot serve dot-directory search at all.
- Everything in scope is indexed — visible, hidden, and Junk. Nothing is excluded; tiers differ only in ranking penalty and refresh priority. Junk is classified by a built-in, user-editable path-pattern list (`node_modules`, `.git`, `target`, `.build`, tool caches, agent session stores). The non-Junk tier updates in real time from filesystem events; Junk refreshes lazily (idle, or a query targeting it) and may be minutes stale (#21).
- Index lifecycle: one full background crawl on first launch (search works on partial data meanwhile); afterwards incremental via filesystem events; process start diff-rescans. A mounted local external volume is crawled on mount, leaves the index on unmount, and persists keyed by volume identity so remount only diff-rescans. Network volumes are never indexed — reachable by browsing, paths, and as Known Places (#21). Sizing evidence: 3.2 M files on the reference machine; in-memory index feasible (#21).
- Ranking guarantees (weights are tuning, not contract): exact name match beats any penalty; RU/EN layout correction, no phonetic transliteration (#8); Visit Journal and Known Places boost; the Alias Dictionary (Settings; e.g. «загрузки» → `~/Downloads`) recommends and never filters; hidden light penalty, Junk heavy penalty; path-shaped scope priority per above. Duplicate worktrees get no structural handling; the Visit Journal lifts the active tree (#21).
- The Visit Journal records Locations entered and files opened through the app, timestamped, local, never user-facing. Record liberally in v1; pruning the schema is prototype work (#21).
- Search Results combine exact paths, visited Locations, current-Location matches, and global Name Index hits; results arrive progressively; reordering stops once keyboard navigation starts (#8). Empty: one "nothing found" line. Slow: one "searching" line. Failures are row states — never a banner, never the Status Strip; the current Location, selection, and scroll always survive; raw backend errors (e.g. `ENOENT`) never reach the UI (#19).
- An inaccessible path shows "no access" as its row state; Enter still attempts entry and on failure follows the #11 policy (System Settings hint). Enter on a file path performs Reveal, never an external open (#19).
- Hidden entries are always visible in directory listings — no toggle; their sort placement is production UX (#10).

## 7. Recents (#10, research #3)

- The Recents collection is system-derived: Spotlight (`mdfind` last-used ≥ 90-day window, widening as needed, per research #3) with batched metadata; files only, no directories; newest last-used first. The Finder-parity gap is accepted; hidden and support files are rejected by the documented post-filter.
- No sorting or filtering controls on the Recents view in v1 (a different order may become a Settings option later). Typing starts global Search, not a filter.
- Progressive loading: first visible rows instantly from the last-success cache, batches of 100 in the background on scroll; no hard cap. Refresh cost evidence: 59–84 ms (#7).
- Inaccessible Items stay visible and fail on action with an explanatory error; the app expects near-complete disk access. Degraded states: honestly empty vs Spotlight unavailable (disabled / privacy-excluded / still indexing) with an explanation — no silent filesystem-crawl fallback.

## 8. Operations and application routing (#11)

- Operation set: Enter/Open, Open With, Quick Look, Copy Path, Copy File, Paste, Move-Paste, Rename, New Folder, Move to Trash, Delete Permanently, Open in Terminal, Open in Editor, Reveal in Finder, Open in New Tab.
- Clipboard: Finder's model. `Cmd+C` puts a file reference (Copy Path is a separate textual action), `Cmd+V` pastes a copy, `Cmd+Opt+V` moves, no cut. Same-Location paste duplicates; name collisions resolve with a suffix. Duplicate is not a separate operation.
- Deletion: `Cmd+Delete` → system Trash, no confirmation, ever; recovery is the system Trash (Finder Put Back) — v1 has no undo system at all. `Opt+Cmd+Delete` deletes permanently, always confirms, no "don't ask again". Rows disappear only after success.
- Opening: macOS system defaults via Launch Services; no per-type override table. Open With lists compatible installed apps; apps stored and resolved by bundle ID.
- Terminal and Editor: two Settings slots (bundle IDs), auto-seeded on first run — system default handler where macOS exposes one, then a known-app priority list (Ghostty → iTerm2 → Terminal; Zed → VS Code → …). Environment variables are not consulted. Open in Terminal: directory itself / file's containing Location. Open in Editor: the file / the directory as project. T3 Code is excluded until it ships a stable directory-open contract.
- The Status Strip is the only feedback channel — the product has no toasts. Batch progress, cloud downloads, and failures (appearing the moment they occur, evolving from progress to a problem list) all live there, including single-action errors.
- Failure policy: validate the path immediately before dispatch; a failure never touches the Tab, selection, or history; concrete cause in the Status Strip, never raw exception text; no automatic Finder fallback. Batches continue past individual failures in the background. A configured-but-missing app keeps its action visible and routes to Settings on invocation; permission denials hint at the relevant System Settings grant.
- Cloud placeholders: v1 draws no placeholder distinction in listings or the Preview Panel. Opening hands the file to macOS to materialize, with download progress in the Status Strip; rich provider states need a native bridge and are post-v1 (#11, research #4).

## 9. Preview Panel and Quick Look (#8)

- The Preview Panel is a persistent fixed-width right column (default on; Settings toggle; no show/hide animation). It follows the Focused Item in Browse and Search Results: files get immediate metadata, then image / text or Markdown excerpt / first PDF page / generic icon; directories get path and metadata without recursive size. Stale preview work never blocks navigation or overwrites a newer selection.
- Space toggles Quick Look (full-document). While open, Up/Down move through files only (selected files under multi-select); closing preserves the last focused file and original selection. Production must own a native `QLPreviewPanel` via the bridge — the `qlmanage` demo workaround is not acceptable (ADR-0001, demo results).

## 10. Performance contract (#12)

Three classes: **Instant** ≤50 ms (indicators forbidden), **Fast** ≤150 ms (indicators forbidden, content may fill in), **Async** >150 ms (progressive in-place rendering; background work reports to the Status Strip). Budgets (reference machine; evidence #7):

| Interaction | Budget |
| --- | --- |
| Warm entry: shortcut → restored window | Instant ≤50 ms |
| Login prewarm (not user-facing) | ≤2 s |
| Manual first start → usable window | ≤800 ms |
| New Temporary Tab → input focused, cached Recents painted | Instant ≤50 ms |
| Ordinary directory (≤~1,000 items), full listing | Fast ≤100 ms |
| Large directory: first screen | ≤150 ms, remainder streams |
| Recents background refresh | ≤150 ms |
| Keystroke response (focus, selection, character) | one 120 Hz frame, ≤8 ms |
| First Search results per keystroke | ≤50 ms |
| Quick Look dispatch | ≤50 ms |

- Below 150 ms nothing is indicated; no blocking overlays or modal waits exist. Any navigation or keystroke cancels in-flight work of the previous state; a stale result never overwrites newer state. One caching pattern: show cached instantly, revalidate in background, update in place without moving the focused row.
- Slow storage (network volumes, undownloaded iCloud) is exempt from the numbers, not the rules: never block, in-place loading past 150 ms, leaving cancels, no hard timeouts.
- Energy: the hidden resident does no periodic work (filesystem events only; idle CPU 0% is a requirement; Activity Monitor energy impact negligible). Heavy work runs at background QoS. On battery, lazy Junk rescans defer until power or a targeting query; the one-time initial crawl runs regardless.
- Verification: production carries local NDJSON telemetry (same shape as the demo). Budgets are checked on the reference machine; no CI perf rig. Before acceptance, measure what #7 could not: large-list render, sustained scroll, activation across Spaces and full-screen, per-show focus confirmation, icon and preview costs, post-reboot start. Framework parity evidence: `prototypes/framework-bench/` (ADR-0002).

## 11. Persistence inventory

| Data | Survives restart | Notes |
| --- | --- | --- |
| Pinned Tabs: Anchor, custom name, order | yes, promptly persisted | #9 |
| Temporary Tabs, Excursions, selection, history | no | #9 |
| Name Index (+ per-volume indexes keyed by volume identity) | yes | #21 |
| Visit Journal | yes, local only | #21 |
| Recents last-success cache | yes | #10, #12 |
| Settings (below) | yes | |
| NDJSON telemetry log | yes, local only | #12 |

## 12. Settings inventory (complete for v1)

Global shortcut (default `Ctrl+Opt+Cmd+F`) · Default Entry Point (default Recents) · Temporary Tab lifetime (default 3 h) · primary action per Item kind (#8 defaults) · After Action table — Hide Window / Keep Open per action; defaults: Hide for Open File, Open in Terminal/Editor, Copy Path, Copy File; Keep Open for Trash, Quick Look, Enter Directory, navigation (#8) · Preview Panel on/off (default on) · Terminal slot, Editor slot (auto-seeded) · Alias Dictionary (editable word → Location) · Junk pattern list (editable) · Pinned Tab management implied by Tab UI. Nothing else is a v1 setting; do not add settings the spec does not name.

## 13. Permissions and platform

- Expect near-complete disk access: first-run guidance requests Full Disk Access and Login Item enrollment; degraded operation without them must still not crash — inaccessible content follows the failure policy (#10, #11).
- TCC grants are app-wide; discover capability per operation, never advertise a root as permanently readable/writable (research #4). Spotlight health only affects Recents (§7). SIP paths and other users' homes simply fail with the concrete cause.

## 14. Non-goals of version 1

No content search · no Spotlight beyond Recents · no sidebar · no undo (`Cmd+Z`) · no Duplicate action · no per-type open overrides · no T3 Code action · no network-volume indexing · no mount management or eject · no rich cloud placeholder states · no toasts · no multi-window, detached Tabs, or per-display memory · no MRU Tab cycling · no Vim keys, type-to-filter, or query history · no CI perf rig · no localization (post-v1 desired) · no marketplace/onboarding/monetization · final visual styling deferred to the first post-v1 update.

## 15. Deliberately unspecified (production-UX freedom)

Exact spacing, typography, colors, and dimensions (with the 300 px Preview Panel and 120–140 pt Tab minimum as baselines); Status Strip placement and animation; hidden-entry sort placement in listings; Tab-overflow feel; empty/degraded state wording (invariants in §6–§7 still bind); suffix format for paste collisions.

## 16. Acceptance

Each cited issue's resolution carries its own acceptance scenarios (#8 §implied, #9, #10, #11, #12, #19, #21); they are all binding. End-to-end smoke on top: from a fresh login, press the shortcut, paste a copied absolute path, Reveal the file, Quick Look it, copy its path, open its directory in the terminal, Trash a scratch file, find a dotfile config by wrong-layout query, pin the directory, hide, wait six minutes, invoke — the Pinned Tab is at its Anchor and everything above met its speed class.

## 17. Suggested implementation sequence

1. Shell: window, activation policy, global shortcut toggle, Login Item, telemetry log — validate warm-entry and prewarm budgets first.
2. Listing and navigation: dense table, virtualization, Browse Mode keys, history, Tabs with lifecycle and persistence.
3. Name Index: crawl, FSEvents incremental updates, persistence, tiers; then the ranker with its guarantees; wire the Navigation Input and Search Results overlay.
4. Recents collection with cache-first paint and degraded states.
5. Operations, clipboard, routing, Status Strip, failure policy.
6. Native bridges: `QLPreviewPanel`, activation polish. Preview Panel.
7. Settings surface, first-run flow (FDA, Login Item, slot seeding).
8. Performance validation pass on the reference machine, including everything #7 left unmeasured.

Steps 1–2 make a usable browser; 3 makes it Beeline. Keep telemetry on from step 1.
