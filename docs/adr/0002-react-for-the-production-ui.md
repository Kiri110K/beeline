---
status: accepted
---

# React for the production UI

The production UI is React (current React with the React Compiler enabled) inside the Tauri WKWebView. Solid was considered seriously because the product's data flow is signal-shaped and the performance contract demands a keystroke answer within one 120 Hz frame. A best-effort A/B benchmark (`prototypes/framework-bench/` at git tag `planning-end`, ticket #22) measured both frameworks on the contract's hot path — focus movement through a 50,000-row virtualized table, streamed insertion, and in-place cache revalidation — and found them statistically indistinguishable: 0.5–2 ms per operation for both, four to sixteen times inside the 8 ms budget. Virtualization keeps the DOM window near 40 rows, which makes framework overhead immaterial.

With performance removed from the equation, the choice falls to familiarity and ecosystem, and both favor React (it is also Kirill's default stack). Solid's remaining advantages are real but do not matter here: its ~5× smaller bundle (14 kB versus 69 kB gzipped in the benchmark builds) is irrelevant for a resident local application that loads its bundle once at login prewarm, and its no-VDOM update model would pay off only on large non-virtualized DOM, which this product does not have by design.

Revisit only if production telemetry shows the keystroke budget failing for framework-attributable reasons, or in a later generation of the product. Migration was deliberately accepted as a future option rather than prevented.
