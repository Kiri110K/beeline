# Beeline — implementation-ready alpha specification

Beeline is a personal, shortcut-driven macOS file browser that replaces Finder for everyday file access: open a copied path, use Recents, browse and preview, copy paths, delete, and open files or directories in the right application — all keyboard-first with complete mouse support. Primary user: Kirill; the reference machine is his Apple Silicon MacBook.

This document synthesizes every resolved decision. Sources of record: the glossary (`/CONTEXT.md` — all capitalized terms below are defined there), ADR-0001 (Tauri host) and ADR-0002 (React UI) in `/docs/adr/`, and the resolution comments of the GitHub issues cited as `#N`. If this document and an issue resolution disagree, this document wins; report the discrepancy.

An implementer must not invent product behavior. Anything genuinely unspecified here is either listed in "Deliberately unspecified" (production-UX freedom) or is a bug in this spec.

## 1. Architecture

- Host: Tauri v2. React (current version, React Compiler enabled) in the WKWebView owns the UI; Rust owns the filesystem, the Name Index, Spotlight queries, the Visit Journal, and system integration (ADR-0001, ADR-0002).
- Small native macOS bridges are allowed where public Tauri APIs fall short. Expected candidates: an owned `QLPreviewPanel` for Quick Look, precise window activation, and (post-v1) iCloud placeholder state (ADR-0001).
- Bundle id `com.kiri110k.beeline`. Beeline is the version-1 working name; the final name is decided before any public release (the bare name is crowded publicly — App Store, the Apache Hive `beeline` CLI, the RU telecom brand). UI language: English. Localization is a desired post-v1 addition, so do not hard-code strings in components.
- The visual style of the selected mockup (`prototypes/navigation-search-preview/design-a-browser.html` at git tag `planning-end`; prototypes are deleted from the working tree) is the compositional reference, not final styling; a style rework is planned as the first post-v1 update.

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

## 6. Search v2 (#42, #43–#51, #54)

### 6.1 Query interpretation

- Every non-empty Navigation Input value is one Search Query. Retrieval may evaluate literal name/path, exact existing path, Alias Dictionary, Keyboard Layout Correction, Typo Correction, ordinary token, and Path Interpretation evidence, but all candidates enter one ranker. There are no mutually exclusive input modes.
- Normalize query identity with NFC, full case folding, trimmed surrounding whitespace, and collapsed internal whitespace. Preserve punctuation. An ordinary Query Family contains the same distinct tokens regardless of order or repetition.
- Path Interpretation preserves component order, treats spaces and slashes as equivalent separators, permits omitted intermediate components, and retains the meaning of `~`, `./`, and `../`. Thus `work wip` may identify `~/work/.../wip`, but reversed components are different evidence.
- Keyboard Layout Correction maps physical keys between supported layouts; it is not transliteration. Typo Correction supports insertion, deletion, substitution, and adjacent transposition. One interpretation may use at most one correction; corrections never compose transitively.

### 6.2 Staged retrieval

- Every query searches the complete Working Set first. Its current-Location component has no Item limit; each historical source is bounded independently. The Working Set is a retrieval pool, not a ranking bonus.
- Global Name Index retrieval begins at five significant characters, inclusive. Significant characters are Unicode letters and digits after NFC; punctuation, spaces, and path separators contribute zero, and token lengths sum. An exact existing path or exact Alias Dictionary match bypasses the threshold. This value is a hot-reloadable Ranker Configuration input; changing it reruns the active query.
- Global retrieval merges production literal/layout candidates with a persisted q-gram fuzzy supplement. The q-gram layer only retrieves candidates; exact code-defined verification produces Candidate Evidence before ranking. Full mmap fuzzy scan is a retained benchmark, not a production fallback. FST is not planned unless measured IPC/render latency or sidecar size establishes a need.
- The Name Index covers names and paths, not file contents. Visible, hidden, and Junk Items remain searchable. Hidden and Junk affect ranking, not eligibility. Non-Junk content follows filesystem events. Junk refresh waits for five seconds of actual quiet or an explicit targeting query and reconciles only changed directory boundaries; continuous activity must not force a subtree rebuild.
- Search works against a partial initial index. Mounted-volume indexing remains deferred to #33; network volumes are browseable but not indexed.

### 6.3 Search Memory and personal evidence

- Eligible interactions accumulate direct evidence. Opening an Action Menu is weak, Quick Look is medium, and a completed non-destructive Item action or entering a directory is strong. An interaction made with a query updates both Search Memory and query-independent learned usage; one without a query updates only learned usage.
- Visibility, scroll, hover, focus, selection, cancellation, failure, Trash, and permanent deletion provide no positive evidence. Repeated events accumulate directly through a monotone saturating curve; there is no special repeat-session heuristic in this version.
- Search Memory always records the exact normalized query and its supported interpretation. Ordinary evidence transfers at full strength within its Query Family. Path evidence remains ordered and allows omitted components. A proven layout correction may transfer. Prefix and one-token add/remove transfer are asymmetric and penalized. Edit transfer allows none for a changed token of 1–4 characters, one edit for 5–8, and up to two edits within 20% for 9+ characters. Only one transformation is applied; transfers are not transitive.
- An Item retrieved solely through Search Memory reinforces only the exact query association when acted on. Stable Item Identity follows rename and same-volume move. Copies, replacements, and cross-volume copies start new identities. Missing or unmounted Items retain dormant evidence but never appear as ghosts; `No Access` applies only to an Item known to exist but currently blocked by permissions.
- Learned evidence ages continuously without a TTL. The shared learned-state budget defaults to 64 MiB; when full, the weakest decayed associations are evicted. Reset Learned Ranking deletes Search Memory and query-independent learned usage, but preserves Recents, Visit Journal, Pinned Tabs, aliases, and the Name Index.

### 6.4 One deterministic ranker

- Retrieval returns Candidate Evidence, including sources and match explanations. One deterministic ranker alone decides order; retrieval source and Working Set membership have zero score by themselves.
- The score is the sum of capped named groups — Text Match, Search Memory, General Usage, Context, Alias, and Item Kind — minus capped Penalties. Item Kind starts neutral. Current Location applies only to direct Items. Hidden and Junk each apply once, share a cap, and remain recoverable by strong direct query evidence.
- Text Match chooses one best complete explanation: one interpretation, no more than one correction, the best hit per token, and material weight for the weakest token. Exact filename is strongest, near-exact stem follows, and extension is weak unless the query contains a dot. Search Memory uses its strongest applicable association. General Usage keeps Recents, Visit Journal, and learned usage contributions separate under one group cap.
- Equal total scores resolve by normalized display name, normalized full path, then Stable Item Identity. With unchanged candidates and configuration, order is repeatable. Increasing a positive raw fact cannot lower its contribution; stronger penalties cannot improve a result.
- `ranker.json` is the sole editable Ranker Configuration. The alpha file is a complete effective config, not a delta. It contains only code-known features, non-negative weights/caps/penalty magnitudes, and validated monotone simple curves. Missing, unknown, non-finite, or semantically invalid values reject the whole file.
- At launch, an absent file uses the embedded default. An invalid file is preserved for diagnosis while the embedded default becomes active. Explicit reload is atomic: invalid input keeps the previous active config; valid input reranks the active Candidate Evidence, and a wave produced with an older config fingerprint cannot overwrite it. There is no file watcher.

### 6.5 Ranked Result Stream and diagnostics

- Each query produces one Ranked Result Stream: complete Working Set, cumulative global waves, then one complete snapshot. A newer query or configuration fingerprint cancels and invalidates older work.
- The first row may be auto-focused, but is not sticky. After deliberate keyboard result navigation, progressive waves preserve only that Focused Item by Stable Item Identity; the remaining order may change. Passive hover and scroll preserve nothing. Any query change resets this preservation.
- Stream state is `local-ready`, `global-running`, or `complete`. No progress UI appears before 150 ms; beyond it the existing Status Strip may report continued global work. Empty and inaccessible Items follow the existing row-state and failure rules.
- Ranking Traces are local and replayable. Every query update records a compact trace; after 300 ms idle or any eligible action, record Candidate Evidence and contributions for the top 256, always including the acted-on Item. Retain traces for 30 days or 256 MiB, whichever limit is reached first. Store each effective config snapshot once by fingerprint while retained traces reference it.
- The ranking CLI validates, explains, replays, compares, and applies configs. `apply` atomically replaces the authoritative file and asks a running app to reload; if it cannot confirm reload, it reports the split state and leaves the new file authoritative for next launch.

## 7. Recents (#10, research #3)

- The Recents collection is system-derived: Spotlight (`mdfind` last-used date through a measured one-year window, with one unbounded fallback when the cache target is not filled) returns paths and dates in one process; files only, no directories; newest last-used first. The Finder-parity gap is accepted; hidden and support files are rejected by the documented post-filter.
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
| First useful Search Results per keystroke, end to end | p95 ≤50 ms |
| Warm complete ranked top 50 | p95 ≤500 ms |
| Quick Look dispatch | ≤50 ms |

- Below 150 ms nothing is indicated; no blocking overlays or modal waits exist. Any navigation or keystroke cancels in-flight work of the previous state; a stale result never overwrites newer state. Search may remain incomplete beyond 500 ms on a cold, rebuilding, or unusually broad path, but it stays progressive and cancellable and must not delay the Working Set wave.
- Slow storage (network volumes, undownloaded iCloud) is exempt from the numbers, not the rules: never block, in-place loading past 150 ms, leaving cancels, no hard timeouts.
- The settled hidden process must stay below 200 MiB physical footprint and startup peak below 400 MiB on the reference machine. The persisted q-gram sidecar may use up to 600 MiB on disk in alpha. A sidecar rebuild is background work and may peak at 700 MiB; the previous usable index remains searchable until atomic replacement.
- Energy: the hidden resident does no periodic polling (filesystem events only; idle CPU rounds to 0%; Activity Monitor energy impact negligible). Continuous filesystem activity may batch bounded incremental work but must not force periodic Junk maintenance. Heavy work runs at background QoS. On battery, lazy Junk rescans and optional index compaction defer until power or a targeting query; the one-time initial crawl runs regardless.
- Search recall is exact for the supported literal, path, layout, and verified typo interpretations over candidates retrieved by the current algorithms. Approximate global retrieval is best-effort outside the committed regression corpus; q-gram hits are always verified, so broad retrieval may omit an unsupported fuzzy candidate but may not fabricate a match.
- Verification: production carries local NDJSON telemetry (same shape as the demo). Budgets are checked on the reference machine; no CI perf rig. Before acceptance, measure what #7 could not: large-list render, sustained scroll, activation across Spaces and full-screen, per-show focus confirmation, icon and preview costs, post-reboot start. Framework parity evidence: `prototypes/framework-bench/` at git tag `planning-end` (ADR-0002).

## 11. Persistence inventory

| Data | Survives restart | Notes |
| --- | --- | --- |
| Pinned Tabs: Anchor, custom name, order | yes, promptly persisted | #9 |
| Temporary Tabs, Excursions, selection, history | no | #9 |
| Name Index (+ per-volume indexes keyed by volume identity) | yes | #21 |
| q-gram fuzzy sidecar | yes, atomically replaceable | rebuildable from Name Index; no compatibility promise |
| Visit Journal | yes, local only | #21 |
| Recents last-success cache | yes | #10, #12 |
| Search Memory and learned usage | yes, local only | 64 MiB shared default; aging and weakest-first eviction |
| `ranker.json` | yes | one authoritative full alpha config; embedded default fallback |
| Ranker config snapshots | while referenced | content-addressed by fingerprint |
| Ranking Traces | yes, local only | 30 days or 256 MiB |
| Settings (below) | yes | |
| NDJSON telemetry log | yes, local only | #12 |

Learned events and traces are buffered off the interaction path; no eligible action waits for a synchronous disk flush. Graceful quit checkpoints pending learned state. A crash may lose the most recent buffer but must not corrupt the previous checkpoint. Indexes, configs, checkpoints, and compacted logs are written to a sibling temporary file and atomically replaced. Search, ranking, learned-state, and trace schema versions are discard boundaries, not migration promises: an incompatible alpha build may preserve the unreadable file for diagnosis, start clean, and continue.

## 12. Settings inventory

Global shortcut (default `Ctrl+Opt+Cmd+F`) · Default Entry Point (default Recents) · Temporary Tab lifetime (default 3 h) · primary action per Item kind (#8 defaults) · After Action table — Hide Window / Keep Open per action; defaults: Hide for Open File, Open in Terminal/Editor, Copy Path, Copy File; Keep Open for Trash, Quick Look, Enter Directory, navigation (#8) · Preview Panel on/off (default on) · Terminal slot, Editor slot (auto-seeded) · Alias Dictionary (editable word → Location) · Junk pattern list (editable) · Pinned Tab management implied by Tab UI · Reload Ranker Configuration · Reset Learned Ranking. Detailed ranking values are edited in `ranker.json`, not duplicated as individual Settings controls.

## 13. Permissions and platform

- Expect near-complete disk access: first-run guidance requests Full Disk Access and Login Item enrollment; degraded operation without them must still not crash — inaccessible content follows the failure policy (#10, #11).
- TCC grants are app-wide; discover capability per operation, never advertise a root as permanently readable/writable (research #4). Spotlight health only affects Recents (§7). SIP paths and other users' homes simply fail with the concrete cause.

## 14. Non-goals of the current alpha

No content search · no Spotlight beyond Recents · no sidebar · no undo (`Cmd+Z`) · no Duplicate action · no per-type open overrides · no T3 Code action · no network-volume indexing · no mount management or eject · no rich cloud placeholder states · no toasts · no multi-window, detached Tabs, or per-display memory · no MRU Tab cycling · no Vim keys, type-to-filter, or query history · no CI perf rig · no localization yet · no marketplace/onboarding/monetization · no compatibility layer for prior search/ranking APIs, weights, index sidecars, traces, or learned state · no final Gen2 interaction shell in this iteration.

## 15. Deliberately unspecified (production-UX freedom)

Exact spacing, typography, colors, and dimensions (with the 300 px Preview Panel and 120–140 pt Tab minimum as baselines); Status Strip placement and animation; hidden-entry sort placement in listings; Tab-overflow feel; empty/degraded state wording (invariants in §6–§7 still bind); suffix format for paste collisions.

## 16. Acceptance

Each cited issue's resolution carries its own acceptance scenarios; they are binding unless this document explicitly supersedes them. End-to-end smoke on top: from a fresh login, press the shortcut, paste a copied absolute path, Reveal the file, Quick Look it, copy its path, open its directory in the terminal, Trash a scratch file, find a dotfile config by wrong-layout query, find `methodology` from `метолология`, find an omitted-component path such as `work wip`, perform an eligible action, repeat the query and observe a traceable Search Memory boost, reload a changed ranker config without restart, reset learned ranking, pin the directory, hide, wait six minutes, invoke — the Pinned Tab is at its Anchor and everything above met its speed class.

## 17. Suggested implementation sequence

1. Shell: window, activation policy, global shortcut toggle, Login Item, telemetry log — validate warm-entry and prewarm budgets first.
2. Listing and navigation: dense table, virtualization, Browse Mode keys, history, Tabs with lifecycle and persistence.
3. Name Index: crawl, FSEvents incremental updates, persistence, tiers, q-gram sidecar; staged retrieval and Ranked Result Stream.
4. Recents collection with cache-first paint and degraded states.
5. Operations, clipboard, routing, Status Strip, failure policy.
6. Native bridges: `QLPreviewPanel`, activation polish. Preview Panel.
7. Search Memory, learned usage, Stable Item Identity, one ranker, `ranker.json`, Ranking Traces, CLI, reload and reset controls. Remove the superseded ranking and persistence paths in the same change; do not retain a v1 fallback.
8. Settings surface, first-run flow (FDA, Login Item, slot seeding).
9. Performance validation pass on the reference machine, including everything #7 left unmeasured.

Steps 1–2 make a usable browser; 3 and 7 make its personal retrieval loop. Keep telemetry on from step 1. Implementation order documents dependencies, not compatibility phases: only one search/ranking path remains active after each replacement.
