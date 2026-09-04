# Path-aware fuzzy parallel threshold

## Decision

Accept the lower threshold for path-aware fuzzy verification. Candidate sets with 20,000 or
more slots use the existing bounded worker pool; cheaper name-only work keeps the previous
50,000-slot threshold.

The change fixes the remaining `work wip` latency outlier without changing retrieval, matching,
ranking, or worker count for the already-parallel large cases.

## Production A/B

Both pairs ran the 11-case production Search v2 suite with 100 samples per case, one warmup,
and the live 5.67-million-Item index. Each pair used one seed for both binaries; the second pair
reversed execution order.

| Metric | Baseline | Candidate | Change |
| --- | ---: | ---: | ---: |
| Suite benchmark, seed 6901 | 19.55 s | 16.00 s | -18.2% |
| Suite benchmark, seed 6902 | 19.65 s | 15.86 s | -19.3% |
| `work wip` total p95, seed 6901 | 45.99 ms | 11.97 ms | -74.0% |
| `work wip` total p95, seed 6902 | 46.11 ms | 11.60 ms | -74.9% |
| User CPU, seed 6901 | 56.93 s | 58.20 s | +2.2% |
| User CPU, seed 6902 | 57.01 s | 57.55 s | +0.9% |
| Retired instructions, seed 6901 | 726.74 B | 728.09 B | +0.2% |
| Retired instructions, seed 6902 | 726.66 B | 728.01 B | +0.2% |

All per-observation top-10 fingerprints matched. Peak physical footprint was unchanged within
noise and slightly lower in both candidate runs. Cases below the new path threshold were
unchanged. Already-parallel cases were flat or faster; their worker policy did not change.

The relevant `work wip` candidate set has 37,194 slots. Under the old shared 50,000-slot
threshold it verified on one thread and spent about 44 ms in verify/rank. The new path-aware
threshold sends it through the existing eight-worker path, reducing verify/rank to 9.9–10.3 ms.
The roughly 1–2% CPU cost across the full suite buys a 34-ms reduction in the user-visible
outlier and keeps its p95 well inside the 50-ms end-to-end budget.

Raw JSON and `/usr/bin/time -lp` outputs are under the directory recorded by
`/private/tmp/beeline-fuzzy-path-threshold-latest-path`.
