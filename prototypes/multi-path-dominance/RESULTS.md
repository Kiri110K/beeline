# Multi-token Path Interpretation dominance

## Decision

Accept the local dominance check. Ordinary multi-token matching now retains the already-computed
quality of the final name token. It reuses that quality for Path Interpretation and skips the
ancestor walk when the ordinary score is at least as strong as the best possible scoped score.

The comparison uses the active ranker weights. There is no hardcoded assumption about the
default numbers. A regression test covers both sides: all-name evidence that dominates scoped
evidence, and a configured score shape where the scoped interpretation must still win.

## Production A/B

Two full 11-case pairs used 100 samples per case, one warmup, the same seed within each pair,
and reversed execution order. Every per-observation top-10 fingerprint matched.

| Metric | Baseline | Candidate | Change |
| --- | ---: | ---: | ---: |
| Suite benchmark, seed 7101 | 16.35 s | 16.29 s | -0.3% |
| Suite benchmark, seed 7102 | 16.12 s | 16.02 s | -0.6% |
| User CPU, seed 7101 | 61.43 s | 59.15 s | -3.7% |
| User CPU, seed 7102 | 59.95 s | 58.74 s | -2.0% |
| Retired instructions, seed 7101 | 728.09 B | 728.14 B | flat |
| Retired instructions, seed 7102 | 727.95 B | 728.03 B | flat |

The individual p95 values moved in both directions during the randomized full suites. A focused
500-sample `status report` run reduced total p95 from 53.03 to 51.19 ms and verify/rank p95 from
46.37 to 44.61 ms. Its total wall time fell 23.11 → 22.76 seconds. User CPU and instructions
were flat in that focused process.

This is a modest latency win rather than another large algorithmic step. It adds no allocation,
keeps ranking output identical, and removes redundant final-name matching plus some ancestor
walks. Raw JSON and `/usr/bin/time -lp` outputs are under the directory recorded by
`/private/tmp/beeline-multi-path-dominance-latest-path`.
