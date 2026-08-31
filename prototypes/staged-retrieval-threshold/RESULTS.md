# Staged retrieval threshold results

## Decision

Use **five significant characters, inclusive**, as the first Search v2 default for adding
the whole Name Index to the Working Set. Keep the value centralized and hot-updatable.

A significant character is one Unicode letter or digit after NFC normalization. Spaces,
slashes, punctuation, underscores, hyphens, `~`, `.` and `..` do not count. Multiple query
tokens contribute to one total, so `work w` has five significant characters and `work wip`
has seven. Digits count like letters; there is no general four-digit exception.

Two forms bypass the threshold:

- an existing explicit path resolves directly, without a global scan;
- an exact Alias Dictionary word injects its Location into the Working Set result stream,
  without waiting for the global threshold.

An incomplete explicit path and a Path Interpretation that does not resolve directly use the
ordinary significant-character count. The whole Name Index joins progressively; Working Set
results remain available while it runs.

## Why five

Four is too eager for this personal corpus. `netw` and `пми в` already found their personal
Items in the Working Set at rank 1 and rank 2 in less than 0.1 ms. Adding the whole index cost
about 20–45 ms and produced 50 rows. Four-character numeric queries cost about 62–69 ms.
`2026` was already saturated inside the Working Set, while global `2035` was dominated by
unrelated archive UUID fragments. A generic numeric exception would add noise.

Six waits one useful character too long. `conte` found the non-Working-Set `CONTEXT.md` at
global rank 2 in 27–34 ms. With a six-character threshold, it remained unavailable until
`contex`. Five admits this kind of specific global target without opening the whole index for
every four-character personal prefix.

The boundary is deliberately a tunable default. Search Memory and real-use logs may show that
Kirill's query distribution favors a different value, and no stored experimental state needs
migration when it changes.

## Corpus and Working Set shapes

The run on 2026-08-31 used the persisted production Name Index v4 at
`/Users/kiri110k/Library/Application Support/com.kiri110k.beeline/name_index/home.idx`:

- 5,244,905 Items;
- 200 cached system Recents plus 29 Visit Journal records, reduced to 209 distinct paths;
- 189 historical paths resolved in the persisted base and 20 did not;
- no real Pinned Anchors and no real Alias Dictionary entries;
- no Search Memory, because Search v2 has not implemented it yet.

Two current-Location shapes were measured:

| Current Location | Direct Items | Historical Items resolved | Working Set total |
| --- | ---: | ---: | ---: |
| `/Users/kiri110k/work` | 27 | 189 | 215 |
| `/Users/kiri110k/work/wip` | 135 | 189 | 321 |

The prototype does not distinguish whether the 20 unresolved historical paths are newer,
removed, outside the home index, or present only in the live overlay. It opens the persisted
base read-only, so this understates the live Working Set rather than overstating it.

## Boundary probes

Each timing is the median of five warm production-ranker runs. Working Set scans used the
`/Users/kiri110k/work` shape; the second shape produced the same threshold verdict.

| Query | Significant | Expected Item | Working Set | Whole Name Index |
| --- | ---: | --- | --- | --- |
| `netw` | 4 | recent `NETWORK-SETUP.md` | rank 1, 0.02 ms | rank 1, 21.98 ms |
| `пми в` | 4 | recent `ПМИ_ver_1_41.html` | rank 2, 0.08 ms | not in first 50, 40.60 ms |
| `hand` | 4 | non-recent `HANDOFF.md` | absent, 0.01 ms | rank 1, 27.46 ms |
| `0002` | 4 | non-recent ADR 0002 | absent, 0.02 ms | rank 1, 68.76 ms |
| `netwo` | 5 | recent `NETWORK-SETUP.md` | rank 1, 0.01 ms | rank 1, 22.01 ms |
| `план т` | 5 | recent `ТЗ_План_ТО.html` | rank 1, 0.07 ms | rank 5, 37.95 ms |
| `conte` | 5 | non-recent `CONTEXT.md` | absent, 0.01 ms | rank 2, 26.60 ms |
| `contex` | 6 | non-recent `CONTEXT.md` | absent, 0.01 ms | rank 2, 22.65 ms |
| `work wip` | 7 | `work/wip` Location | rank 1, 0.07 ms | rank 2, 32.89 ms |
| `work/wip` | 7 | `work/wip` Location | rank 1, 0.01 ms | rank 1, 17.62 ms |

Across the 17 non-bypass global probes, median full-index time was 32.89 ms. The observed
range was 16.20–68.76 ms and p90 was 63.50 ms. The slow four-digit cases reinforce the staged
design: the Working Set answers first, and the global path should not block it.

## Counting and bypass contract

| Query | Significant characters | Behavior at default 5 |
| --- | ---: | --- |
| `work wip` | 7 | add Name Index |
| `work/wip` | 7 | add Name Index |
| `2026` | 4 | Working Set only |
| `план то` | 6 | add Name Index |
| `./a1-b2` | 4 | Working Set only unless it resolves directly |
| `../` | 0 | direct navigation if resolvable; otherwise Working Set only |
| `~/.config` | 6 | direct navigation if it exists |
| synthetic alias `bee` | 3 | inject alias target; no global scan required |

The synthetic alias `bee → /Users/kiri110k/lab/beeline` appeared at Working Set rank 1 in
0.02–0.03 ms. The explicit path `/Users/kiri110k/lab/beeline` returned directly in about
0.003 ms. These checks use a synthetic alias only because the real dictionary was empty.

## Hot update

The prototype kept the active query `conte` unchanged and applied this threshold sequence:

| New threshold | Active scope immediately after update |
| ---: | --- |
| 6 | Working Set only |
| 5 | Working Set plus Name Index |
| 4 | Working Set plus Name Index |
| 6 | Working Set only |

The assertion passed without retyping or replacing the active query. The production contract
should therefore recompute the active scope when the setting changes and cancel or start the
global branch as needed.

## Limits of this prototype

- It reuses the current production ranker only to measure corpus and candidate shape. Search
  v2 ranking and Path Interpretation will replace that behavior.
- The current first-4096-candidates cap can hide a relevant Item from the first 50 global
  rows. The typo-retrieval research already requires rank-aware top-k before the ranker.
- The current production ranker has no general typo retrieval. `метолология` returned zero
  rows even after scanning all 5,244,905 Items. The staged threshold cannot fix that; the
  global fuzzy architecture belongs to the typo-retrieval implementation.
- Measurements are warm, local, and specific to this Mac. They choose the product boundary,
  not the final global retrieval data structure or ranking weights.
