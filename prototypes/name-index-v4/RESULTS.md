# Name Index v4 prototype results

## Production outcome

The selected design is now implemented directly as Name Index v4. There is no v3 migration
or compatibility path: a non-v4 index is rejected and the ordinary initial crawl replaces it.

Production v4 adds the pieces the storage prototype deliberately omitted:

- mapped fixed-width directory and Item records with one UTF-8 arena;
- base tombstones, owned overlay entries and directories, path lookup across both stores,
  and mtime overrides;
- reachable-tree compaction, streaming persistence, checksum and structural validation,
  atomic replacement, and clean-index write skipping;
- the real parallel ranker over mapped base plus overlay;
- a watcher startup gate: FSEvents are registered before the crawl, queued during it, and
  applied only after v4 compaction, preventing a duplicate subtree crawl from starving Search.

The final real-app pass on 2026-08-25 rebuilt 5,150,527 entries in 101.757 seconds. The first
v4 file was 309,859,773 bytes. Both `процедура приемки` and `ghjwtlehf ghbtvrb` returned two
results through the real UI in 64/58 ms end-to-end and 57/56 ms in the backend. A restart
loaded 5,150,533 entries without another crawl: 982 ms from launch request to `index_loaded`,
and 578 ms between the process telemetry records.

After the restart search, RSS was 419 MiB because it included 308 MiB of clean mapped pages.
Charged physical footprint settled at 68.6 MiB and stayed flat over four samples. The full
independent report is `/private/tmp/codex-computer-use.beeline-v4-fixed.08afZo/report.md`.

Measured on the reference Mac on 2026-08-25 against:

- source: `/Users/kiri110k/Library/Application Support/com.kiri110k.beeline/name_index/home.idx`;
- v3 file size: 294,395,506 bytes (280.8 MiB);
- 5,099,664 Items and 505,209 directory nodes.

The production v3 baseline loaded in 491.49 ms. Its standard full-result searches were:

| Query | Production v3 median |
| --- | ---: |
| `zzz_no_such_entry_zz` | 1.46 ms |
| `процедура приемки` | 21.94 ms |
| `ghjwtlehf ghbtvrb` | 21.68 ms |
| `kirill macbook` | 18.02 ms |

The production ranker uses eight shards. The prototype scan below is sequential and checks
candidate membership rather than final ranking, so its timings are not a replacement for the
production benchmark.

## Candidate image

The converter produced:

- 303,378,019 bytes (289.3 MiB);
- 5,099,664 Items;
- 418,635 reachable directory nodes;
- 86,574 orphan directory nodes removed;
- no live Items removed;
- 164.6 MiB UTF-8 name arena;
- deterministic content hash `b2578378f05332da`.

The prototype's 24-byte Item record makes the image 8.5 MiB larger than v3 on disk. It is
still much smaller than v3's 504.7 MiB measured in-memory lower bound because it removes the
per-name allocations, `Option<Entry>` padding, per-directory maps, and per-directory vectors.
A production format can trim the record further, but that is not needed to decide between
heap and `mmap`.

## Heap versus mmap

| Measurement | Compact heap | Read-only `mmap` |
| --- | ---: | ---: |
| Warm open | 131.24 ms | 2.82 ms |
| Full validation | 443.98 ms | 449.30 ms |
| Physical footprint after all scans | 297 MB | 1.4 MB |
| Peak physical footprint during scans | 297 MB | 8.0 MB |
| RSS after all scans | 291 MiB | 291 MiB |

The mapped image's RSS consists almost entirely of clean file-backed pages. macOS excludes
those reclaimable pages from process physical footprint. The heap copy is dirty anonymous
memory and remains charged to the process.

One later `mmap` run faulted the full image during validation and took 1.15 seconds. It still
fits the two-second hidden-prewarm budget, but this is only a warm-machine observation. A
post-reboot measurement is still required.

## Candidate scan

Both readers returned identical candidate counts and hashes on every query:

| Query | Heap median | mmap median | Candidates | Hash |
| --- | ---: | ---: | ---: | --- |
| `g` | 98.39 ms | 96.94 ms | 1,152,959 | `bf65b05e0191edde` |
| `gh` | 41.36 ms | 42.71 ms | 47,588 | `a5d34c69dea845d1` |
| `zzz_no_such_entry_zz` | 18.14 ms | 18.17 ms | 0 | `cbf29ce484222325` |
| `процедура приемки` | 34.75 ms | 36.00 ms | 2 | `4eedf9d731716680` |
| `ghjwtlehf ghbtvrb` | 34.71 ms | 35.66 ms | 2 | `4eedf9d731716680` |
| `kirill macbook` | 38.47 ms | 38.65 ms | 566 | `e11aeff84559b76b` |

Storage choice does not measurably affect a warm scan over the same bytes. The common
one-character query exceeds 50 ms because the prototype intentionally scans every candidate
sequentially; production stops at its candidate cap and uses eight shards.

## Mutable overlay

The mapped-base prototype was also tested with:

- 10,000 base tombstones;
- 5,000 renamed Items represented as tombstone plus addition;
- 5,000 new Items;
- 1.84 MiB total overlay allocation;
- 1.21 ms to apply all 15,000 logical mutations.

The two synthetic overlay queries returned exactly 5,000 candidates each. A miss returned
zero. Their sequential full scans took 30–31 ms. The tombstone check and 10,000 added records
therefore fit inside the 50 ms search budget in this storage-only prototype.

This prototype overlay does not model directory creation, path lookup, or atomic rebuild.
Production v4 now does. One mapped base per mounted local volume remains future product work.

## Decision

Use a read-only mapped base plus a small mutable overlay for v4.

The compact heap variant already meets the 400 MiB target at 297 MB. The mapped base keeps
the same scan speed while removing roughly 296 MB from charged process footprint and opens
without copying the 289 MiB image. That margin matters once mounted local volumes are added.

The production implementation settles the overlay, atomic replacement, and real-ranker
integration. Mounted-volume ownership remains separate future work. Ranked-result behavior is
covered through the existing ranker contract tests rather than a v3 runtime comparison, in
line with the direct-replacement decision.
