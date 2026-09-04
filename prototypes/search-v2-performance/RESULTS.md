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
identity. It should not show progress for the measured warm path. At this prototype boundary,
IPC and rendered merge were the remaining unknown because the slowest first wave left only about
10 ms inside the 50 ms end-to-end contract; the integration below closes that measurement gap.

## Production integration — 2026-09-03

The prototype architecture now runs in the signed Tauri bundle through one channel-backed
Search v2 stream. The frontend paints the complete Working Set, cumulative q-gram shard waves,
and the deterministic final ranking. Only deliberate keyboard focus is preserved by stable Item
path; a query edit clears that preservation and stale-query waves are rejected.

Live end-to-render measurements over the current 5.5M-Item index:

| Query | First useful paint/callback | Final paint/callback | Outcome |
| --- | ---: | ---: | --- |
| `метолология` | 17 ms | 51 ms | `МЕТОДОЛОГИЯ.md` rank 1 |
| `ьуерщвщдпн` | 36 ms | 288 ms | layout + typo results found |
| `work wip` | 17 ms | 480 ms | `/Users/kiri110k/work/wip` rank 1 |

The slower final waves were measured while startup diff-rescan was active. The binding first
useful result remains within 50 ms for the measured typo, layout-plus-typo, and implicit-path
cases; broad completion can continue beyond the 150 ms no-indicator boundary. The current
overlay therefore reorders quietly as shards land. That observed tail, rather than the Rust-only
prototype, is the input for the next UI iteration.

The live mutable overlay was much larger than the prototype corpus assumed. A cheap prepared
name filter now reduces expensive verification (for example, 280,911 candidate slots to 9,965
verified Items for `метолология`) while preserving the accepted rule that an ordinary
multi-token candidate has at least one plausible name-token match. Ordered implicit Path
Interpretation competes as a strong score contribution rather than a forced first result.

Cold sidecar construction is isolated in a CLI-only helper process: 14.692 s for 128,438,403
postings and a 522,142,268-byte file. The GUI remained responsive, and its post-build physical
footprint was about 80 MB. Building inside the GUI had taken 114.572 s at background QoS and
left about 644 MB in retained allocator pages, so that path was rejected.

## Randomized production-core benchmark — 2026-09-03

The integrated production path was measured in a new headless CLI mode. The mode exits before
Tauri initialization and therefore never creates a window. It loads the real Name Index, q-gram
sidecar, Visit Journal, aliases, Recents, current Location, and production matcher/ranker.

Method:

- three independent processes before and after the change;
- 200 observations per case per process, 600 per case in the combined distribution;
- one warm-up per case, then all 2,000 observations shuffled by a different fixed seed;
- AC power; `/usr/bin/time -lp` around every session;
- top-10 result identity fingerprint and target rank checked on every observation.

The baseline sessions took 294.2, 290.9, and 284.4 seconds. Sampling showed
`bounded_osa_chars` together with repeated allocation/free as the dominant verification stack.
The accepted change replaced three per-comparison `Vec<usize>` allocations and the full DP
matrix with reusable per-worker `u16` rows and a `2k+1` diagonal band. An additional Myers
prefilter was measured and rejected because its extra string pass made the suite slower.

Combined 600-observation p95 timings, milliseconds:

| Query | First useful, before → after | Verify/rank, before → after | Final, before → after | Final speedup |
| --- | ---: | ---: | ---: | ---: |
| `skills` | 0.50 → 0.25 | 48.85 → 19.55 | 54.99 → 25.60 | 2.15× |
| `метолология` | 0.45 → 0.25 | 7.35 → 3.96 | 7.99 → 4.41 | 1.81× |
| `methodolgy` | 23.47 → 15.99 | 24.56 → 5.47 | 38.22 → 18.92 | 2.02× |
| `methodoology` | 17.71 → 12.69 | 8.91 → 3.76 | 17.71 → 12.69 | 1.40× |
| `methdoology` | 31.30 → 22.03 | 25.16 → 5.90 | 43.48 → 24.26 | 1.79× |
| `ьуерщвщдщпн` | 35.73 → 25.49 | 27.44 → 6.13 | 48.94 → 28.09 | 1.74× |
| `ьуерщвщдпн` | 23.20 → 16.25 | 24.11 → 5.42 | 37.38 → 18.99 | 1.97× |
| `work wip` | 0.92 → 0.55 | 358.17 → 145.12 | 363.89 → 150.46 | 2.42× |
| `vault methodology` | 249.14 → 78.35 | 302.09 → 91.38 | 327.27 → 118.28 | 2.77× |
| `status report` | 1.85 → 0.88 | 631.02 → 197.19 | 657.50 → 224.12 | 2.93× |

The optimized sessions took 105.9, 105.6, and 110.6 seconds: 2.70× less aggregate wall time.
Peak physical footprint stayed within 69–70 MiB after the change versus 61–68 MiB before; the
difference is small and there was no retained-memory growth across sessions. Every query kept
one deterministic top-10 fingerprint across all samples, and every optimized fingerprint
matched the baseline fingerprint.

A separate reconciled run measured the current 298,588-slot in-memory overlay after an
8.39-second diff-rescan. Its p95 target/final times were 8.49 ms for `метолология`, 31.83 ms for
`ьуерщвщдпн`, 203.44 ms for `work wip`, 196.85 ms for `vault methodology`, and a 0.995 ms
Working Set target followed by a 420.63 ms final wave for `status report`. This state confirms
that broad completion still belongs in quiet progressive reranking; the common typo/layout
targets are already comfortably below 50 ms in the production core.

Two labeled targets (`skills-drafts` and the dated `status-report` workbook) appear in the
Working Set but fall outside the final global top 50. That is a ranking/merge-quality input for
Search Memory tuning, not a retrieval failure. The benchmark records both the early target and
the final miss rather than hiding the distinction.

## Visible paint and follow-up tuning — 2026-09-04

A visible pass through the installed signed bundle closed the IPC → React → paint boundary on
the optimized implementation. The first non-empty/final painted waves were 9/60 ms for
`метолология`, 37/344 ms for `ьуерщвщдпн`, 12/333 ms for `work wip`, 225/258 ms for
`vault methodology`, and 17/453 ms for `status report`. The physical footprint stayed at
75 MiB across a ten-second idle sample and fell to 73 MiB after the hidden app sat idle for
several minutes; the observed peak was 88 MiB. No settled stale rows, duplicates, focus jumps,
or freezes appeared.

The pass exposed two concrete defects rather than a general React or IPC cost:

- `vault methodology` painted an empty Working Set quickly, then waited for a 141,178-candidate
  typo-safe final-token pool before it could show a valid ordered-path result;
- Working Set membership selected candidates early but did not reach final scoring as retrieval
  evidence, and all-name multi-token matches could rank below a name-plus-ancestor Path
  Interpretation. `skills-drafts` and the dated `status-report` workbook therefore disappeared
  from the final top 50.

The production path now intersects every literal q-gram posting for the ordered final token and
fully verifies that narrow pool before global typo retrieval. The mutable overlay gets the same
literal-tail scan because it is not in the sidecar. Working Set sources remain separate
Candidate Evidence for the ranker (`current Location`, `Pinned Anchor`, and `Recents`), and a
direct all-tokens-in-name interpretation has its own score contribution. None of these paths
forces a fixed row position; one deterministic ranker still produces every wave.

A 20-sample randomized run over all ten cases after a 7.22-second reconciliation added 302,714
overlay slots. Results:

- `vault methodology` narrowed the early pool to 80 Items; priority retrieval/verification p95
  was 1.87/0.67 ms and the labeled target arrived at 3.40 ms p95 instead of 191.78 ms;
- `skills-drafts` remained rank 1 in every final result instead of missing every final top 50;
- the dated `status-report` workbook remained in every final result at rank 8 instead of missing
  every final top 50;
- every case had one deterministic top-10 fingerprint across the run, every labeled target was
  present in every final result, and first-useful p95 was at most 32.33 ms;
- the slowest final p95 was 372.91 ms for `status report`; broad completion remains a quiet,
  cancellable progressive phase rather than part of the 50 ms first-result contract;
- peak physical footprint was 89,048,264 bytes.

The benchmark report schema is now version 2. It adds priority-retrieval duration, priority
verification duration, priority candidate count, and priority result count, so later agent-led
tuning can distinguish the early ordered-tail path from global candidate retrieval.

An adaptive shared-directory-mask follow-up was also measured and rejected. Building one dense
mask table before broad q-gram verification reduced duplicated shard state, but moved too much
directory work onto one thread. In back-to-back 10-sample loaded sessions, `vault methodology`
final p95 regressed from 212.86 to 304.95 ms and `status report` from 545.04 to 758.03 ms; only
`work wip` improved, from 309.15 to 283.35 ms. Production therefore keeps lazy shard-local path
masks. This does not rule out a later concurrent sparse/shared design, but an eager dense table
is not the next optimization.

A later automation pass that deliberately avoided taking foreground focus is not a valid paint
benchmark: WKWebView deferred its `requestAnimationFrame` callbacks until several minutes after
the searches. It did confirm final visible rows and a 60 MiB settled physical footprint, but its
paint timings are discarded. Foreground visual revalidation of this exact build remains pending.

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
- Search Memory, energy, incremental sidecar maintenance, and post-reboot page-fault behavior
  remain unmeasured. IPC/render timing and stale-result rejection are now covered by the
  production integration and its reducer contract test.
