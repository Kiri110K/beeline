# Framework benchmark results (ticket #22)

Run 2026-08-23, headless WebKit 26.5 via Playwright (the production host is a Tauri WKWebView — same engine family), Apple Silicon. Three rounds per framework, identical scenarios, identical row markup and styles, TanStack Virtual in both.

Best effort per contender:

- React 19 with the React Compiler enabled (memo-cache markers verified in the production bundle), explicit `memo` on the row, `flushSync` on the discrete-input path.
- Solid 1.9 with idiomatic signals and `<For>`.

## Numbers

`sync` = script + reconciliation + DOM + forced layout, in ms (p50/p95/max across 3 rounds). This is the part that spends the 8 ms keystroke budget (#12). The `paint` column of the raw data is floored at ~33 ms by the headless 30 Hz requestAnimationFrame cadence and is not meaningful here; real-display validation stays with the production build.

| Scenario | React (compiler) | Solid |
| --- | --- | --- |
| focus-move, 300 steps through 50k virtualized rows | 1 / 1 / 3 | 0–1 / 1 / 1 |
| streamed insertion, 49 batches of 1,000 rows | 1 / 2 / 2 | 1 / 2 / 2 |
| cache revalidation, 50k swap with 2% changed, 20 reps | 0 / 2 / 2 | 0 / 2 / 2 |

Sanity checks: both apps finish with the same focused row (`item-000120`, revalidated size applied) and the same 42-row virtualized DOM window.

## Reading

The frameworks are statistically indistinguishable on the contract's hot path. With a virtualized window of ~40 rows, per-update framework overhead is 0.5–2 ms for both — four to sixteen times inside the 8 ms budget. Performance does not decide this choice; it should be made on other grounds (familiarity, ecosystem, taste).

## Caveats

- Headless WebKit; rAF at 30 Hz floors the paint metric. The sync metric is unaffected.
- Scenarios cover the specified hot path only. Nothing here measures Preview Panel content rendering or non-virtualized layouts, which the product does not plan to have.
- Raw data: `bench-results.json`. Reproduce: build both apps (`bun install && bun run build` in each), then `node run-bench.mjs`.
