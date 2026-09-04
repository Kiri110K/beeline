# ASCII q-gram fast path

## Decision

Accept the change. Most production Item names are ASCII, but the builder was sending all of them
through general Unicode NFC composition and lowercase expansion. A byte-based ASCII path cuts
rebuild wall by 48-51%, user CPU by 58%, and retired instructions by 58%. Non-ASCII names keep
the existing Unicode path.

The production search suite also uses less CPU and returns exactly the same results.

## Implementation

`gram_buckets_into` first checks `str::is_ascii`. ASCII names use byte lowercase and ASCII
alphanumeric token boundaries with the same rolling three-character hash. Non-ASCII names keep
the NFC, Unicode lowercase, and Unicode alphanumeric implementation unchanged. Both paths still
sort and deduplicate buckets per Item.

The hash is identical because every ASCII byte has the same Unicode scalar value and the existing
hash consumes that scalar as four little-endian bytes. The full production sidecar comparison
checks this property across all indexed names.

## Paired production rebuilds

Input: 5,668,822 base slots and 136,481,293 postings. The second pair reverses binary order.

| Pair | Unicode path for all names | ASCII fast path | Change |
| --- | ---: | ---: | ---: |
| Wall, baseline first | 8.67 s | 4.48 s | -48.3% |
| User CPU, baseline first | 6.60 s | 2.75 s | -58.3% |
| Instructions, baseline first | 91,957,409,102 | 38,650,910,469 | -58.0% |
| CPU cycles, baseline first | 22,357,715,820 | 10,212,028,153 | -54.3% |
| Wall, candidate first | 7.55 s | 3.68 s | -51.3% |
| User CPU, candidate first | 6.60 s | 2.74 s | -58.5% |
| Instructions, candidate first | 92,105,607,648 | 38,729,611,533 | -58.0% |
| CPU cycles, candidate first | 22,491,976,656 | 10,244,504,865 | -54.5% |

Physical memory was unchanged at about 67.2 MiB. Every output was 185,963,243 bytes with
SHA-256 `90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`, exactly matching the
installed production sidecar.

## Paired search benchmark

Both binaries ran the 11-case production Search v2 suite with 100 samples per case, one warmup,
and the same seed within each pair. The second pair reversed execution order.

| Pair | Baseline wall | Candidate wall | Baseline user CPU | Candidate user CPU |
| --- | ---: | ---: | ---: | ---: |
| seed 7501, baseline first | 16.13 s | 15.92 s | 57.63 s | 56.45 s |
| seed 7502, candidate first | 16.47 s | 16.21 s | 58.35 s | 57.21 s |

Candidate retired instructions decreased by 0.31% in both pairs, and CPU cycles decreased by
1.6-2.1%. Every one of the 2,200 observations kept the same candidate count, posting visits,
verified Items, target rank, result count, and top-10 fingerprint. This includes Cyrillic typo,
wrong-layout, and every path case. All targets were found.

## Verification

- Rust: 144 tests passed and 12 host-dependent tests ignored.
- Clippy passed with warnings denied.
- Frontend contracts, TypeScript build, Vite production build, and ESLint passed.
- The existing reference-equivalence test covers ASCII case, digits, punctuation, repeated grams,
  Cyrillic, decomposed Unicode, lowercase expansion, and emoji separators.

The profile that motivated this branch is under the directory recorded by
`/private/tmp/beeline-qgram-profile-latest-path`. Benchmark artifacts are under the directory
recorded by `/private/tmp/beeline-qgram-ascii-fast-latest-path`.
