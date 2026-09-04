# Q-gram checkpoint stride 128

## Decision

Reject the change. Doubling the checkpoint stride from 64 to 128 saves 8,503,760 bytes, but the
current sidecar is already well below its 600-MiB budget. The one standard case that exercises
literal checkpoint lookup became about 0.13 ms slower at p95 in both paired runs. Suite CPU and
wall time did not improve reproducibly.

The experimental code remains on remote branch `experiment/qgram-skip-stride-128`. Main keeps
the 64-posting stride and only this result.

## Production measurements

Input: 5,668,822 base slots and 136,481,293 postings from the live home Name Index.

| Metric | Stride 64 | Stride 128 |
| --- | ---: | ---: |
| Sidecar bytes | 185,963,243 | 177,459,483 (-4.6%) |
| Checkpoint records | 2,159,052 | 1,096,082 |
| Rebuild wall | 17.46-18.69 s in preceding builder runs | 18.38 s |
| Rebuild physical peak | 67.8 MiB | 67.63 MiB |

The final posting stream remains 160,302,147 bytes. Only the checkpoint table shrinks.

## Paired query benchmark

Both binaries ran the 11-case production Search v2 suite with 100 samples per case, one warmup,
and the same seed within each pair. The second pair reversed execution order.

| Pair | Stride 64 wall | Stride 128 wall | Stride 64 user CPU | Stride 128 user CPU |
| --- | ---: | ---: | ---: | ---: |
| seed 7201, 64 first | 17.40 s | 16.31 s | 57.25 s | 57.06 s |
| seed 7202, 128 first | 16.10 s | 16.05 s | 56.41 s | 56.78 s |

Retired instructions increased by 0.035% and 0.024% with stride 128. Every one of the 2,200
measured observations kept the same candidate count, posting visits, verified Items, target rank,
result count, and top-10 fingerprint. All targets were found.

`gapped-path` is the only standard case with an ordered-tail literal priority lookup. Its
priority-retrieval p95 changed from 0.192 to 0.319 ms at seed 7201 and from 0.185 to 0.316 ms at
seed 7202. Final end-to-end timing stayed within the existing contract, but spending extra query
CPU to save 8.1 MiB is the wrong trade while the sidecar has more than 400 MiB of budget left.

The six focused q-gram tests and Clippy with warnings denied passed. Artifacts are under the
directory recorded by `/private/tmp/beeline-qgram-skip128-latest-path`.
