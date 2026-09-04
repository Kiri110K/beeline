# Single-decode q-gram build shards

## Decision

Accept the change. The builder already knows every bucket's exact posting count from its first
Name Index pass, so recounting all delta-encoded temporary records was redundant. Reusing the
global counts removes one decode of about 422 MiB per rebuild and consistently saves about 1% of
retired instructions. Wall time improved by 2.7-3.3% in the paired implementation runs.

The final sidecar format and query path do not change.

## Implementation

- Each shard derives its local counting-sort offsets directly from the existing global bucket
  offsets.
- The temporary records are decoded once into their pre-sized bucket ranges.
- The decoder rejects an out-of-range bucket, too many records for one bucket, too few records
  for any bucket, a total count mismatch, malformed varints, and non-monotone slots.
- A focused test supplies correct and deliberately wrong per-bucket counts to the same helper
  used by the production builder.

## Paired production rebuilds

Input: 5,668,822 base slots and 136,481,293 postings from the live home Name Index. Both pairs
use the same mapped index and reverse binary order in the second pair.

| Pair | Baseline | Single decode | Change |
| --- | ---: | ---: | ---: |
| Wall, baseline first | 18.65 s | 18.14 s | -2.7% |
| User CPU, baseline first | 16.53 s | 16.35 s | -1.1% |
| Instructions, baseline first | 267,244,814,288 | 264,685,410,288 | -1.0% |
| Wall, candidate first | 17.84 s | 17.26 s | -3.3% |
| User CPU, candidate first | 16.74 s | 16.15 s | -3.5% |
| Instructions, candidate first | 266,911,953,558 | 264,150,912,041 | -1.0% |

Physical memory stayed effectively flat at about 67.8 MiB. Max RSS varied with filesystem cache
order and did not move consistently.

After extracting the validation into the directly tested helper, a final production rebuild took
18.20 seconds, retired 262,323,340,487 instructions, and reached a 67.67-MiB physical peak. Its
output remained 185,963,243 bytes.

All five outputs had SHA-256
`90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`, exactly matching the
installed production `BLQGM003` sidecar.

## Verification

- Rust: 143 tests passed and 12 host-dependent tests ignored.
- Clippy passed with warnings denied.
- Frontend contracts, TypeScript build, Vite production build, and ESLint passed.
- Seven focused q-gram tests cover the intermediate record format, per-bucket validation, final
  sidecar construction, candidate retrieval, cancellation, and varint boundaries.

Artifacts are under the directory recorded by
`/private/tmp/beeline-qgram-single-decode-latest-path`. It contains the compared binaries, timing
files, hashes, and final byte-identical sidecars.
