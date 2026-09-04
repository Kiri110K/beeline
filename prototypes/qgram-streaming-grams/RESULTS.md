# Streaming q-gram extraction

## Decision

Accept the change. Extracting normalized trigrams directly from the Unicode character stream and
reusing one bucket vector removes millions of temporary strings and vectors during a production
rebuild. Rebuild wall falls by 23-27%, user CPU by 28%, and retired instructions by about 35%.
The production search suite keeps identical results and slightly fewer instructions.

The q-gram hash, final `BLQGM003` sidecar, and retrieval behavior do not change.

## Implementation

- NFC normalization and lowercase expansion remain in the same order as before.
- Non-alphanumeric characters reset a rolling two-character window, matching the old normalized
  token split.
- Each following character emits one stack-built trigram hash.
- The output bucket vector is still sorted and deduplicated per Item.
- The builder clears and reuses one vector across all 5.67 million Items. Query retrieval keeps
  the same small allocating wrapper because it runs only once per token.

A unit test compares the streaming implementation against the old normalized-token reference for
Latin, Cyrillic, decomposed Unicode, lowercase expansion, digits, repeated grams, punctuation,
and emoji separators.

## Paired production rebuilds

Input: 5,668,822 base slots and 136,481,293 postings. The second pair reverses binary order.

| Pair | Baseline | Streaming grams | Change |
| --- | ---: | ---: | ---: |
| Wall, baseline first | 11.10 s | 8.10 s | -27.0% |
| User CPU, baseline first | 9.14 s | 6.57 s | -28.1% |
| Instructions, baseline first | 143,108,820,743 | 92,606,059,257 | -35.3% |
| CPU cycles, baseline first | 30,436,747,172 | 22,410,389,929 | -26.4% |
| Wall, candidate first | 10.18 s | 7.80 s | -23.4% |
| User CPU, candidate first | 9.09 s | 6.55 s | -27.9% |
| Instructions, candidate first | 142,569,916,889 | 92,940,671,037 | -34.8% |
| CPU cycles, candidate first | 30,208,945,899 | 22,439,616,803 | -25.7% |

Physical memory fell by 0.3-0.5 MiB to 67.2 MiB. Every output was 185,963,243 bytes with
SHA-256 `90be476b47b843840b9a2c3007dc0361ca83ce84fe8077139d99c0350aa43945`, exactly matching the
installed production sidecar.

## Paired search benchmark

Both binaries ran the 11-case production Search v2 suite with 100 samples per case, one warmup,
and the same seed within each pair. The second pair reversed execution order.

| Pair | Baseline wall | Candidate wall | Baseline user CPU | Candidate user CPU |
| --- | ---: | ---: | ---: | ---: |
| seed 7401, baseline first | 16.44 s | 16.01 s | 57.37 s | 56.94 s |
| seed 7402, candidate first | 16.18 s | 16.10 s | 57.02 s | 56.92 s |

Retired instructions decreased by 0.37% and 0.36%. Candidate physical peak was 0.6-0.8 MiB
higher; this did not reproduce in the rebuild process and remains far below the product budget.
Every one of the 2,200 measured observations kept the same candidate count, posting visits,
verified Items, target rank, result count, and top-10 fingerprint. All targets were found.

## Verification

- Rust: 144 tests passed and 12 host-dependent tests ignored.
- Clippy passed with warnings denied.
- Frontend contracts, TypeScript build, Vite production build, and ESLint passed.

Artifacts are under the directory recorded by
`/private/tmp/beeline-qgram-streaming-grams-latest-path`. It contains both binaries, rebuild and
search timings, output hashes, equivalence diffs, and the final byte-identical sidecar.
