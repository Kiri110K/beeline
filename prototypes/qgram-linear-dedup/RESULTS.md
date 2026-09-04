# Linear q-gram deduplication

## Decision

Reject the change. Checking the short output vector before every q-gram append removes the final
sort from the builder and lowers measured builder CPU cycles, but it executes 6-7% more
instructions. Rebuild wall changed direction between paired runs. The shared query helper also
became slightly more expensive before restoring the original sorted bucket order.

The experimental code remains on remote branch `experiment/qgram-linear-dedup`. Main keeps the
sort-and-dedup implementation and only this result.

## Paired production rebuilds

All four outputs were 185,963,243 bytes with the production SHA-256
`90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`.

| Pair | Sorted baseline | Linear dedup | Change |
| --- | ---: | ---: | ---: |
| Wall, baseline first | 4.72 s | 4.34 s | -8.1% |
| User CPU, baseline first | 2.72 s | 2.50 s | -8.1% |
| Instructions, baseline first | 38,594,708,062 | 41,194,607,018 | +6.7% |
| CPU cycles, baseline first | 10,064,899,387 | 9,412,706,306 | -6.5% |
| Wall, candidate first | 3.30 s | 3.52 s | +6.7% |
| User CPU, candidate first | 2.70 s | 2.49 s | -7.8% |
| Instructions, candidate first | 38,591,119,757 | 41,004,069,992 | +6.3% |
| CPU cycles, candidate first | 10,039,358,895 | 9,432,358,949 | -6.0% |

Physical memory was unchanged at about 67.2 MiB.

## Paired search benchmark

Both binaries ran the 11-case production Search v2 suite with 100 samples per case and reversed
order in the second pair. All 2,200 observations kept the same candidate count, posting visits,
verified Items, target rank, result count, and top-10 fingerprint.

Process-wide candidate instructions increased by 0.037% at seed 7601 and 0.023% at seed 7602.
Wall, user CPU, and cycles changed direction with execution order. Rebuilds are rare and already
finish in a few seconds, so this trade does not justify replacing the predictable sort with
quadratic worst-case work on long names.

Eight focused q-gram tests and Clippy with warnings denied passed. Artifacts are under the
directory recorded by `/private/tmp/beeline-qgram-linear-dedup-latest-path`.
