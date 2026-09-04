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
the searches. Its paint timings are discarded.

A foreground pass on the exact ordered-tail build then confirmed the intended visible ordering:
`skills-drafts` was rank 1, the Graphify methodology Item was rank 1 for
`vault methodology`, and the dated status-report workbook was rank 8. The first non-empty
Working Set paint was 18 ms for `skills`; `vault methodology` painted its empty Working Set at
20 ms and its useful priority wave at 56 ms; `status report` painted ten useful Working Set rows
at 13 ms. No stale rows, duplicate exact paths, or incorrect target ordering appeared. Broad
completion still exceeded the observation window for the latter two queries, consistent with
the headless final-tail measurements.

That pass also exposed a long-session overlay defect. One completed `skills` search prepared
1,901,798 candidates even though a freshly reconciled session needed roughly 346k. Search was
supplementing the immutable q-gram base with every overlay slot below its historical high-water
mark, including removed build artifacts. Repeated filesystem churn also kept appending new slots
and stale per-parent references. The watcher included Beeline's own app-data directory, so
telemetry and journal writes generated needless filesystem batches while the whole home was
watched.

The mutable overlay now exposes only live slots to both priority and global retrieval, reuses
removed Item and directory holes, removes stale parent membership, and trims removed tails.
Beeline's app-data subtree is excluded at the watcher callback, preventing internal persistence
from feeding the index. Churn regressions cover slot reuse, directory-node reuse, parent
correctness, and the live-slot iterator.

A post-fix reconciled run contained 335,015 overlay slots. Across 30 targeted observations,
the three labeled targets were found every time: `work wip` rank 1,
`vault methodology` rank 5 in the headless empty-memory state, and the status-report workbook
rank 8. Candidate pools were 369,177 for `work wip` and about 503k for both broad multi-token
queries, rather than the 1.9M long-session pool. First-useful p95 was 0.59, 3.79, and 0.99 ms;
final p95 was 198.32, 211.09, and 420.74 ms respectively. In the newly installed hidden bundle,
physical footprint settled at 74.8 MiB after startup, with a 105.5 MiB startup peak. The app
remained hidden with no windows during this verification.

## Junk refresh long-session follow-up — 2026-09-04

The remaining watcher traffic was captured at the filesystem boundary rather than inferred from
batch telemetry. Over 30 seconds the home watcher received 194 path notifications for 55 distinct
paths. None was inside Beeline's app-data subtree or one of its ancestors. The largest sources were
T3 trace logs, T3/Chromium LevelDB and caches, Telegram, WhatsApp, and Teams. The app-data exclusion
therefore works; the continued event stream is real activity from other applications.

The expensive behavior was the Junk refresh policy. Over 4 hours 19 minutes, the previous installed
build recorded 3,816 watcher batches. Of those, 3,060 made no index mutation. Batch application
accounted for 92.55 seconds in total, but the largest no-op batch waited 24.96 seconds for the index
write lock. Four forced Junk rescans ran after the fixed 30-second deadline and took 43.00 seconds,
1,188.84 seconds, 1,288.17 seconds, and 305.51 seconds. Continuous cache/log activity therefore
caused repeated whole-subtree rebuilds instead of postponing maintenance.

The worker no longer forces a refresh during continuous traffic. Every Junk event restarts the
five-second quiet window; an explicit Junk-targeting query still refreshes immediately. When a
refresh does run, it reconciles only direct children of each dirty directory. It preserves stable
descendant slots, crawls only genuinely new directories, handles file/directory type changes, and
verifies a captured directory identity after an ancestor update. The obsolete clear-and-recrawl
path was removed.

A read-only release run against the persisted 5,244,905-Item production base reconciled the whole
`~/Library/Application Support` Junk root in 57.94 ms and found five new direct Items. The prior
algorithm could select the same high-level dirty root and spend 19–21 minutes rebuilding its full
subtree. The new regression suite has 112 passing Rust tests and nine ignored machine/system
tests; the dedicated ignored live timing also passed. Clippy with warnings denied, frontend
contracts, lint, typecheck, production build, and signed bundle all pass.

The updated installed bundle remained hidden with no windows. After startup reconciliation its
physical footprint was 60.6 MiB with a 66.8 MiB peak. During the first three minutes it handled 82
batches in 605 ms total, with no Junk refresh; the machine was on battery, so this run validates the
deferred path rather than the external-power quiet drain.

## Dense q-gram aggregation — 2026-09-05

Production q-gram postings are sorted by Item slot. The old retrieval path nevertheless built one
`HashMap<u32, u8>` per token and a global `BTreeSet<u32>`. The measured replacement uses one bounded
reusable `Vec<u8>` counter table plus a touched-slot list. It clears only counters touched by the
current token, appends qualifying slots to a flat vector, then sorts and deduplicates once. A pool
retains at most one 5.7M-byte workspace; overlapping cancelled queries may allocate another, but it
is dropped when the pooled workspace returns.

A same-seed 200-sample-per-case A/B run on the current 5,668,822-Item base measured:

- complete suite: 127.62 s to 101.32 s, a 1.26x speedup;
- candidate retrieval p95: 3.8x to 7.8x faster across all ten cases;
- typo/layout final p95: 1.03x to 2.56x faster;
- peak physical footprint: 91.44 MiB to 90.90 MiB;
- identical final hits, one top-10 fingerprint per case, and zero target misses.

A second 100-sample-per-case process with a different seed kept retrieval p95 between 0.055 and
4.625 ms and peaked at 67.95 MiB physical footprint. Cancellation reuse and two concurrent readers
produce the same candidate set in the Rust suite. The installed signed bundle then returned
`МЕТОДОЛОГИЯ.md` at rank 2 for `метолология`, a methodology result at rank 1 for `ьуерщвщдпн`, and
`/Users/kiri110k/work/wip` at rank 1. No stale final rows, duplicates, freeze, focus jump during
typing, or crash appeared. The retained query remained visible after the first Escape as required
by SPEC §5; a second real system Escape hid the window and recorded `origin: escape`.

The next isolated builder experiment reduced the count, offset, and write-position tables from
`u64` to checked `u32` while retaining 64-bit offsets in the existing sidecar format. A full build
produced a byte-identical 554,313,828-byte file. Wall time changed from 18.21 to 17.57 seconds and
peak physical footprint from 543.88 to 536.09 MiB, a 7.78 MiB reduction. This is a small safe win;
the raw 520-MiB postings vector remains the builder's dominant memory cost.

## Reused fuzzy ancestor paths — 2026-09-05

Multi-token fuzzy matching uses the same lowercase directory chain for the direct token-set match,
the ordered Path Interpretation, and every corrected keyboard-layout variant. The old path rebuilt
that owned string vector for every interpretation. The replacement builds it once per candidate
and shares the immutable slice across all interpretations.

A same-seed 100-sample-per-case A/B run measured the complete suite at 51.60 seconds before and
44.07 seconds after the change: a 1.17x speedup and 14.6% less wall time. User CPU time fell 16.5%,
retired instructions fell 12.0%, and CPU cycles fell 16.4%. The largest final-stage p95 changes
were on queries that inspect directory chains: implicit path improved from 169.15 to 145.74 ms,
gapped path from 100.72 to 81.28 ms, and direct multi-token from 211.42 to 173.58 ms. Two typo cases
regressed by less than 1.5%, within run noise. A different-seed repeat finished in 44.12 seconds and
kept the three path-heavy p95 values at 145.77, 89.82, and 184.87 ms. Both runs returned identical
final hits and fingerprints with zero target misses. Peak physical footprint was effectively flat:
67.78 MiB before, 67.56 MiB after, and 64.17 MiB in the repeat process.

## Reused fuzzy lowercase buffers — 2026-09-05

The next verifier pass removed two more repetitions. Each candidate Item name is now lowercased
into the worker's existing reusable string buffer, then shared by direct, path, and corrected-layout
interpretations. Directory components produced by `ancestor_names` are already lowercase, so the
component verifier no longer allocates and lowercases a second copy for every token comparison.

Against the ancestor-reuse result above, the same-seed suite fell from 44.07 to 35.70 seconds: a
1.23x speedup and 19.0% less wall time. User CPU fell 21.5%, retired instructions 19.9%, and cycles
21.5%. Every case improved. Final-stage p95 fell 19.0% for implicit path, 21.8% for gapped path,
and 25.4% for direct multi-token; the seven other cases improved by 3.0% to 10.8%. A different-seed
repeat finished in 35.50 seconds. Both runs preserved every final hit and top-10 fingerprint with
zero target misses. Peak physical footprint was 63.83 MiB in the same-seed process and 68.58 MiB in
the repeat, versus 67.56 and 64.17 MiB for their respective baselines; there is no stable memory
change.

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
- Search Memory, incremental sidecar maintenance, post-reboot page-fault behavior, and a longer
  external-power idle sample after the Junk-refresh fix remain unmeasured. IPC/render timing and
  stale-result rejection are covered by the production integration and its reducer contract test.
