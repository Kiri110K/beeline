# Ten path-aware fuzzy workers

## Decision

Reject. Keep eight workers for path-aware fuzzy verification. Two extra workers made every
measured p95 slower, increased CPU, and added about 8 MiB of physical memory.

The experiment changed only the q-gram path-aware fuzzy worker cap from eight to ten. Full-index
scans remained capped at eight. The 11-case production suite ran 100 samples per case with the
same seed and live 5.67-million-Item index.

| Metric | Eight workers | Ten workers | Change |
| --- | ---: | ---: | ---: |
| Benchmark time | 16.03 s | 16.06 s | +0.2% |
| Process real time | 16.28 s | 17.47 s | +7.3% |
| User CPU | 57.46 s | 59.00 s | +2.7% |
| Physical peak | 69.14 MiB | 77.24 MiB | +8.10 MiB |
| Retired instructions | 728.10 B | 729.08 B | +0.1% |
| Cycles | 174.48 B | 176.25 B | +1.0% |
| `work wip` total p95 | 11.30 ms | 13.17 ms | +16.6% |
| `status report` total p95 | 47.32 ms | 48.47 ms | +2.4% |

All per-observation top-10 fingerprints matched, so this is a pure performance rejection. The
extra path-mask caches explain the physical-memory step, while the uniformly slower case p95
matches the existing observation that this workload saturates memory bandwidth at eight workers.

Raw JSON and `/usr/bin/time -lp` outputs are under the directory recorded by
`/private/tmp/beeline-fuzzy-path-ten-shards-latest-path`.
