# Delta-encoded q-gram build shards

## Decision

Accept the builder change. It cuts the production rebuild's peak temporary q-gram files from
about 797.42 MiB to 438.33 MiB (-45.0%) and lowers the measured physical memory peak from
88.11 MiB to 67.8 MiB (-23%). Three production rebuilds took 17.46-18.69 seconds, so the extra
varint decoding did not produce a rebuild-time regression against the 19.23-second fixed-record
baseline.

The final `BLQGM003` sidecar format and the query path do not change. This only replaces the
builder's temporary six-byte `(local bucket, slot)` records.

## Builder format

- The builder still walks the immutable Name Index in ascending slot order and partitions every
  q-gram posting into one of 64 bucket shards.
- Each temporary record stores a two-byte local bucket plus an unsigned LEB128 slot delta from
  the preceding record in that shard.
- Multiple q-grams from one Item can produce a zero slot delta. The decoder accepts it.
- The decoder rejects partial records, overflowing slot deltas, out-of-range local buckets, and
  a record count that disagrees with the first counting pass.
- The builder reads each shard twice: once to count local buckets and once to counting-sort slots
  for final sidecar encoding. Temporary files remain bounded and are removed after the build.

## Production measurements

Input: 5,668,822 base slots and 136,481,293 postings from the live home Name Index.

| Metric | Fixed six-byte records | Delta records |
| --- | ---: | ---: |
| Peak temporary parts | 797.42 MiB calculated | 438.33 MiB observed |
| Rebuild wall | 19.23 s | 18.69 / 17.46 / 17.53 s |
| Rebuild physical peak | 88.11 MiB | 67.92 / 67.77 / 67.77 MiB |
| Rebuild max RSS | 421.08 MiB | 400.89 / 400.75 / 399.92 MiB |
| Final sidecar | 185,963,243 bytes | 185,963,243 bytes |

The fixed-record temporary size is exact: 136,481,293 postings times six bytes plus
17,272,416 bytes of checkpoint records. The delta-record figure is the maximum sampled `du`
size of the 64 shard files and the growing checkpoint file, so filesystem block rounding is
included.

All three candidate rebuilds produced SHA-256
`90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`. This exactly matches
the already installed production sidecar. The candidate therefore preserved every encoded
posting, bucket offset, and checkpoint byte.

## Verification

- Rust: 142 tests passed and 12 host-dependent tests ignored.
- Clippy passed with warnings denied.
- Frontend contracts, TypeScript build, Vite production build, and ESLint passed.
- Dedicated tests round-trip boundary deltas, repeated slots, and the maximum `u32` slot. They
  also cover non-monotone writes and partial input.

Artifacts are under the directory recorded by
`/private/tmp/beeline-qgram-delta-shards-latest-path`. The retained sidecar is byte-identical to
the installed copy; the two disposable repeat sidecars were removed after comparison.
