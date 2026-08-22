# Prototype Navigation Input, Search Results, and Preview Panel

Type: prototype
Status: resolved
Assignee: /root
Blocked by: 07

## Question

What concrete layout makes the resolved navigation contract feel obvious and fast? Run one `claude -p` session with Opus 5 and have it create three browser-openable interactive mockups rather than production code. The designs must use genuinely different compositions, not one layout with three themes. Each must let Kirill react to Browse Mode, active and retained Navigation Input states, the floating Search Results layer, first-result focus, Reveal for files and directories, range and non-contiguous selection, fixed right-side Preview Panel, full Quick Look transition, Action Menu with and without Selected Items, and the complete Escape sequence. Include realistic dense Recents and directory data, RU/EN query examples, loading and empty search states, narrow-window behavior, and no decorative animation on the fast path.

The prototype resolves layout, sizing, visible state cues, and discoverability. It does not choose search implementation, production components, colors, branding, or visual polish.

## Prototype run

One `claude -p --model opus` process produced three self-contained interactive HTML files:

- `prototypes/navigation-search-preview/design-a-browser.html` — browser-like tabs, full-width Navigation Input, dense table, fixed 300 px Preview Panel.
- `prototypes/navigation-search-preview/design-b-command.html` — quiet file list with a compact top trigger that expands into a command-oriented Search Results sheet.
- `prototypes/navigation-search-preview/design-c-workbench.html` — three visible work surfaces: context rail, current Location, and Preview Panel.

Published review copies:

- https://artifact.kiri110k.workers.dev/70d7eb8ef9ab19f8/design-a-browser.html
- https://artifact.kiri110k.workers.dev/70d7eb8ef9ab19f8/design-b-command.html
- https://artifact.kiri110k.workers.dev/70d7eb8ef9ab19f8/design-c-workbench.html

T3 Preview smoke checks at 1280 x 800 covered query entry, first-result focus, keyboard movement, Reveal, retained query state, persistent Preview Panel, Quick Look, and the Action Menu transition. The wrong-layout example `цщкл` resolves to `work` in the command design. All three also fit an 820 px-wide desktop viewport without document-level horizontal overflow. Mobile and touch behavior are explicitly outside scope.

Kirill selected design A as the product's base composition: browser-like Tabs, full-width Navigation Input, dense file table, and fixed right-side Preview Panel. Designs B and C remain disposable explorations rather than competing implementation directions. Exact styling and pixel dimensions remain outside this prototype decision.
