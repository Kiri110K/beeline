# Search v2 performance prototype results

## Decision

Use three retrieval stages for the first integrated Search v2:

1. Search the complete Working Set immediately.
2. Run the production exact/layout matcher over Name Index v4.
3. Add fuzzy candidates from a mapped q-gram sidecar and rerank the merged set.

Do not prototype an FST yet. The q-gram run met the current latency contract on the real
corpus. Its p95 first-global-result time stayed below 40 ms in every labeled case, and its
slowest final top-50 merge was 136.04 ms. FST becomes relevant only if integrated IPC/render
measurement breaks the 50 ms first-result budget, if the sidecar size proves unacceptable, or
if the q-gram recall limitation survives the next implementation pass.

The full mmap fuzzy scan remains useful as a baseline, but it is not a viable global Typo
Correction implementation. Selective typo queries stabilized in 68–152 ms. Common and
multi-token queries needed 434–603 ms to stabilize and up to 1.13 seconds to finish.

## Corpus and method

The warm run on 2026-09-03 used the production Name Index v4 and live personal state:

- 5,244,905 Items and 426,755 directories;
- 315,639,096-byte Name Index;
- current Location `/Users/kiri110k/work/wip`, containing 135 direct Items;
- 181 resolved historical Items from Recents and the Visit Journal;
- 313 distinct Items in the combined Working Set;
- no Pinned Anchors file and no real Search Memory distribution.

Every reported distribution contains ten measured runs after one unreported warm-up per
query. The harness uses the same query matrix, Candidate Evidence builder, grouped prototype
ranker, rank-aware per-shard top-k, and merge code for both retrieval strategies. It imports
the production Name Index v4 reader and exact/layout matcher directly from the application.

The timings stop at the Rust boundary. They do not include Tauri IPC or WKWebView rendering.
Post-reboot behavior also remains unmeasured.

## Full mmap baseline

Times are p50 / p95 milliseconds.

| Query | First global wave | Stable top 10 | Final top 50 | Target rank |
| --- | ---: | ---: | ---: | ---: |
| `skills` | 14.56 / 14.60 | 14.58 / 14.62 | 344.75 / 351.74 | 1 |
| `метолология` | 34.51 / 35.45 | 67.85 / 69.97 | 137.16 / 139.94 | 28 |
| `methodolgy` | 73.02 / 79.84 | 143.01 / 151.76 | 144.97 / 151.78 | 34 |
| `methodoology` | 66.67 / 71.44 | 138.68 / 143.09 | 138.71 / 143.12 | 34 |
| `methdoology` | 68.35 / 78.23 | 136.62 / 144.84 | 136.64 / 144.86 | 34 |
| `ьуерщвщдщпн` | 12.88 / 12.93 | 12.90 / 12.95 | 141.01 / 142.81 | 4 |
| `ьуерщвщдпн` | 75.99 / 83.92 | 144.18 / 152.16 | 144.21 / 152.18 | 34 |
| `work wip` | 32.19 / 34.57 | 433.69 / 476.09 | 808.01 / 868.76 | 1 |
| `vault methodology` | 33.92 / 34.23 | 33.93 / 34.24 | 584.76 / 609.46 | 2 |
| `status report` | 39.47 / 40.57 | 570.47 / 602.72 | 1090.33 / 1129.24 | 7 |

Working Set retrieval stayed between 0.36 and 1.48 ms at p95. The production exact/layout
wave itself took 12.06–38.51 ms at p95. The remaining delay came from relaxed filtering,
Typo Correction, ancestor matching, and merging after another scan of the whole index.

The ten-case run took 41.08 seconds. `/usr/bin/time -l` reported 366,886,912 bytes maximum
RSS and a 50,561,696-byte peak physical footprint. The mapped Name Index accounts for most of
the RSS and is reclaimable file-backed memory.

## Q-gram candidate retrieval

The q-gram prototype indexes deduplicated normalized filename trigrams into 1,048,576 hashed
buckets. It maps the sidecar read-only, counts shared grams, then runs the exact same bounded
Damerau-OSA verification and ranker only on the retained Item slots.

Times are p50 / p95 milliseconds.

| Query | First global wave | Stable top 10 | Final top 50 | Candidates | Target rank |
| --- | ---: | ---: | ---: | ---: | ---: |
| `skills` | 14.66 / 16.88 | 14.68 / 16.90 | 30.94 / 34.07 | 43,251 | 1 |
| `метолология` | 13.96 / 13.99 | 14.92 / 14.98 | 15.55 / 15.61 | 1,453 | 28 |
| `methodolgy` | 28.46 / 29.66 | 33.52 / 34.83 | 35.51 / 37.30 | 93,592 | 34 |
| `methodoology` | 24.33 / 27.93 | 24.93 / 28.71 | 25.18 / 28.78 | 10,051 | 34 |
| `methdoology` | 38.98 / 40.48 | 46.44 / 48.21 | 46.47 / 48.23 | 118,598 | 34 |
| `ьуерщвщдщпн` | 14.36 / 16.41 | 14.38 / 16.43 | 46.39 / 49.55 | 141,178 | 4 |
| `ьуерщвщдпн` | 28.79 / 34.83 | 34.43 / 39.05 | 36.12 / 40.22 | 93,592 | 34 |
| `work wip` | 32.70 / 33.85 | 62.52 / 65.41 | 66.05 / 70.18 | 33,853 | 1 |
| `vault methodology` | 34.01 / 34.13 | 34.03 / 34.13 | 93.23 / 94.44 | 167,895 | 2 |
| `status report` | 39.44 / 39.61 | 131.24 / 135.93 | 131.35 / 136.04 | 168,215 | 7 |

The labeled target remained in the final top 50 for all ten cases. Search quality tuning is
still needed: the methodology typo variants rank 28 or 34 because many mirrored copies share
the same text evidence and no real Search Memory signal exists yet. This is a ranking problem,
not a retrieval miss.

The first sidecar build took 14.79 seconds and produced 128,438,403 postings in a
522,142,260-byte file. That build run reached 904,167,424 bytes maximum RSS and a
583,730,568-byte peak physical footprint. A warm ten-case query run with the existing sidecar
took 6.42 seconds, reached 375,439,360 bytes maximum RSS, and had a 54,739,976-byte peak
physical footprint.

Cancellation is checked in the production exact scan, q-gram posting walk, and candidate
verification. Replacing `methodolgy` after about 1.29 ms was observed 0.15 ms later and the
superseded run returned `aborted`, so its results can be discarded.

## Result-stream implication

The measurements support a real staged stream rather than a loading state:

- Working Set rows can be ranked in about 0.4–1.5 ms in the Rust core.
- Exact and layout-corrected global rows arrive in roughly 14–40 ms p95.
- Typo-corrected rows arrive and stabilize within 49 ms p95 for the labeled typo cases.
- Broad fuzzy supplements may rerank the list until 136 ms p95, but remain below the existing
  150 ms no-indicator boundary.

The UI should paint each completed wave and preserve only the explicitly Focused Item by stable
identity. It should not show progress for the measured warm path. IPC and rendered merge still
need an integrated pass because the slowest first wave leaves only about 10 ms inside the
50 ms end-to-end contract.

## Known limits

- The hashed trigram overlap rule passed the labeled matrix but has no proof of exhaustive
  recall for every allowed two- or three-edit query. Production work must either strengthen
  that rule, add a bounded completeness path, or explicitly make global fuzzy recall
  best-effort.
- The sidecar stores raw 32-bit postings. Delta-varint compression and incremental overlay
  updates are not implemented. The measured 498 MiB sidecar is therefore an upper baseline,
  not the selected on-disk format.
- The harness uses empty Search Memory and Alias Dictionary state. It resolves live Recents
  and Visit Journal paths only as Working Set and ranking signals.
- IPC, render cost, energy, incremental sidecar maintenance, stale-result rejection in the UI,
  and post-reboot page-fault behavior remain for the integrated implementation pass.
