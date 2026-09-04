# Dense q-gram build deduplication

## Decision

Reject the change for main. A one-byte marker per q-gram bucket removes per-Item sorting and makes
the rare sidecar rebuild 18-21% faster, but sharing the new raw parser with query extraction
increased process-wide Search v2 instructions by 0.08-0.13% in both paired runs. The current
rebuild already finishes in a few seconds, so even a tiny hot-query cost is the wrong trade.

The experimental code remains on remote branch `experiment/qgram-dense-dedup`. It is worth
revisiting only if rebuild frequency becomes material or the builder gets a separate parser without
duplicating normalization logic.

## Paired production rebuilds

All four outputs were 185,963,243 bytes with production SHA-256
`90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`.

| Pair | Sorted baseline | Dense marker | Change |
| --- | ---: | ---: | ---: |
| Wall, baseline first | 4.93 s | 4.04 s | -18.1% |
| User CPU, baseline first | 2.76 s | 2.07 s | -25.0% |
| Instructions, baseline first | 38,450,779,391 | 34,024,964,070 | -11.5% |
| CPU cycles, baseline first | 10,142,363,305 | 8,175,192,936 | -19.4% |
| Wall, candidate first | 3.52 s | 2.80 s | -20.5% |
| User CPU, candidate first | 2.71 s | 2.01 s | -25.8% |
| Instructions, candidate first | 38,466,362,744 | 34,130,195,047 | -11.3% |
| CPU cycles, candidate first | 10,017,667,143 | 7,898,325,454 | -21.2% |

The dense marker adds one MiB during rebuild. Measured physical peak rose from about 67.2 to
68.2 MiB, still far below the contract.

## Paired search benchmark

Both binaries ran the 11-case production Search v2 suite with 100 samples per case and reversed
order in the second pair. All 2,200 observations kept the same candidate count, posting visits,
verified Items, target rank, result count, and top-10 fingerprint.

Candidate instructions increased by 0.126% at seed 7701 and 0.085% at seed 7702. Wall and user
CPU were mixed with execution order. The query algorithm still sorts and deduplicates exactly as
before; the small regression comes from routing it through the shared raw-parser helper.

Nine focused q-gram tests and Clippy with warnings denied passed. Artifacts are under the directory
recorded by `/private/tmp/beeline-qgram-dense-dedup-latest-path`.
