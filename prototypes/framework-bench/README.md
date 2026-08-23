# Framework bench

Disposable A/B benchmark for ticket #22: React (with React Compiler) versus Solid on the performance contract's hot path — a virtualized 50,000-row table under focus movement, streamed insertion, and in-place cache revalidation.

- `shared/bench-core.js` — framework-agnostic scenario driver and metrics; both apps run the same code against an adapter.
- `react-app/`, `solid-app/` — identical UI, per-framework best effort.
- `run-bench.mjs` — headless WebKit runner (Playwright), three rounds per app.
- `RESULTS.md` — measured results and caveats.

```sh
(cd react-app && bun install && bun run build)
(cd solid-app && bun install && bun run build)
node run-bench.mjs > bench-results.json
```
