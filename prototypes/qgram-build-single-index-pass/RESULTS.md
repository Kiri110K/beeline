# Single-pass q-gram source scan

## Decision

Accept the change. The sharded builder can count postings and write temporary records during the
same ordered Name Index scan. Removing the second normalization and q-gram hashing pass cuts
rebuild wall time by 41-42%, user CPU by about 44%, and retired instructions by about 46% on the
production index.

The final `BLQGM003` sidecar, temporary record format, memory bound, and query path do not change.

## Implementation

The builder creates its 64 bounded shard writers before scanning the mapped Name Index. For each
Item, it normalizes the name and computes its distinct q-grams once. The same loop increments the
bucket count and appends the `(local bucket, slot delta)` record. After the writers flush, the
existing checked offset construction and one-decode counting sort produce the final sidecar.

Slot order remains monotone because the source scan remains ordered. An error still drops the
`BuildParts` guard and removes all temporary shards.

## Paired production rebuilds

Input: 5,668,822 base slots and 136,481,293 postings from the live home Name Index. Both pairs
use the same mapped index and reverse binary order in the second pair.

| Pair | Baseline | Single source pass | Change |
| --- | ---: | ---: | ---: |
| Wall, baseline first | 18.13 s | 10.60 s | -41.5% |
| User CPU, baseline first | 16.22 s | 9.08 s | -44.0% |
| Instructions, baseline first | 263,835,566,374 | 143,847,268,769 | -45.5% |
| CPU cycles, baseline first | 52,611,034,308 | 30,342,932,622 | -42.3% |
| Wall, candidate first | 17.13 s | 10.05 s | -41.3% |
| User CPU, candidate first | 16.26 s | 9.19 s | -43.5% |
| Instructions, candidate first | 264,494,982,455 | 143,523,423,582 | -45.7% |
| CPU cycles, candidate first | 52,998,890,917 | 30,804,707,119 | -41.9% |

Physical memory stayed within 67.69-68.00 MiB across all four runs. Max RSS stayed within
400.7-401.0 MiB. Peak temporary parts remain about 438 MiB because this change removes CPU work,
not records.

Every output was 185,963,243 bytes with SHA-256
`90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`, exactly matching the
installed production sidecar.

## Verification

- Rust: 143 tests passed and 12 host-dependent tests ignored.
- Clippy passed with warnings denied.
- Frontend contracts, TypeScript build, Vite production build, and ESLint passed.
- Seven focused q-gram tests cover temporary encoding, per-bucket counts, final construction,
  retrieval, cancellation, and varint boundaries.

Artifacts are under the directory recorded by
`/private/tmp/beeline-qgram-single-index-pass-latest-path`. It contains both compared binaries,
four timing files, output hashes, and the final byte-identical sidecar.
