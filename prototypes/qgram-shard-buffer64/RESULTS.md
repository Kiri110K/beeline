# 64-KiB q-gram shard write buffers

## Decision

Accept the change. Raising each of the 64 temporary shard buffers from Rust's default 8 KiB to
64 KiB reduces rebuild system CPU by 29-32%, retired instructions by 3.9-4.4%, and CPU cycles by
4.5-5.3%. It adds 2.3-3.6 MiB to the measured physical peak during the short rebuild.

This is builder-only. The sidecar format and query path do not change.

## Paired production rebuilds

Input: 5,668,822 base slots and 136,481,293 postings. The second pair reverses binary order.

| Pair | 8-KiB buffers | 64-KiB buffers | Change |
| --- | ---: | ---: | ---: |
| Wall, baseline first | 5.07 s | 4.34 s | -14.4% |
| System CPU, baseline first | 0.51 s | 0.36 s | -29.4% |
| Instructions, baseline first | 38,515,505,722 | 37,017,757,768 | -3.9% |
| CPU cycles, baseline first | 9,970,176,358 | 9,437,469,974 | -5.3% |
| Wall, candidate first | 3.83 s | 3.86 s | +0.8% |
| System CPU, candidate first | 0.59 s | 0.40 s | -32.2% |
| Instructions, candidate first | 38,568,613,155 | 36,879,309,397 | -4.4% |
| CPU cycles, candidate first | 10,386,220,025 | 9,925,158,681 | -4.4% |

User CPU was effectively neutral and changed direction between pairs. Wall time includes filesystem
cache noise and also changed direction, while system CPU, instructions, and cycles improved in both
orders. Physical peak moved from 67.1-67.3 MiB to 69.6-70.6 MiB.

All four outputs were 185,963,243 bytes with production SHA-256
`90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`.

## Verification

- Rust: 144 tests passed and 12 host-dependent tests ignored.
- Clippy passed with warnings denied.
- Frontend contracts, TypeScript build, Vite production build, and ESLint passed.
- Four full production rebuilds produced byte-identical sidecars.

Artifacts are under the directory recorded by
`/private/tmp/beeline-qgram-shard-buffer64-latest-path`.
