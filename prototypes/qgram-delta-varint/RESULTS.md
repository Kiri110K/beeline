# Delta-varint q-gram sidecar

## Decision

Accept the format change. The production Name Index sidecar falls from 554,313,828 to
194,351,859 bytes (-64.9%) without changing candidate recall or final ranking. Query CPU is
effectively neutral at suite level; the small retrieval cost is bounded and well below one
millisecond in every measured case.

This is an alpha-only format replacement. `BLQGM002` deliberately rejects `BLQGM001` and
starts the existing background rebuild. There is no compatibility reader or migration path.

## Format

- Each bucket remains sorted by Name Index slot.
- The first slot is encoded as an unsigned LEB128 absolute value; subsequent slots are
  unsigned LEB128 deltas.
- A checkpoint every 64 postings stores the absolute slot and the byte position after that
  posting. Literal intersection binary-searches the checkpoint table and decodes at most one
  64-posting block.
- Two 64-bit bucket-offset tables locate encoded postings and checkpoints. The header retains
  the source content hash and source entry count.
- The existing 64-shard builder remains bounded. It streams encoded postings and checkpoint
  records instead of materializing a global posting vector.

## Production measurements

Input: 5,668,822 base slots and 136,481,293 postings from the live home Name Index.

| Metric | Fixed `u32` (`BLQGM001`) | Delta-varint (`BLQGM002`) |
| --- | ---: | ---: |
| Sidecar bytes | 554,313,828 | 194,351,859 |
| Rebuild wall | 18.41 s | 19.37 s |
| Rebuild physical peak | 79.83 MiB | 95.94 MiB |
| Rebuild max RSS | 412.88 MiB | 428.94 MiB |

The size prediction made directly from the old sidecar was 194,351,835 bytes. The 24-byte
difference is the expanded v2 header, so the implementation matches the offline model exactly.
Temporary shard files are removed after the build.

## Paired query benchmark

The production headless Search v2 suite ran all 11 cases with 100 samples per case, one warmup,
the same live index/application data, and the same seed within each pair. The second pair
reversed execution order.

| Pair | Fixed suite wall | Varint suite wall | Fixed user CPU | Varint user CPU |
| --- | ---: | ---: | ---: | ---: |
| seed 6505, fixed first | 19.55 s | 20.01 s | 56.20 s | 56.81 s |
| seed 6506, varint first | 19.81 s | 19.79 s | 56.87 s | 57.25 s |

All 22 case comparisons had zero target misses, unchanged posting-visit counts, and identical
top-10 fingerprints. Candidate retrieval p95 increased by 0.03–0.17 ms depending on the case.
End-to-end case p95 was mixed because verifier/ranker work dominates: the apparent large
path-case movements changed sign when execution order was reversed.

An independent streaming verifier decoded every bucket and compared all 136,481,293 postings
plus every checkpoint against the fixed-width production sidecar; all values matched.

Artifacts are under the directory recorded by
`/private/tmp/beeline-qgram-varint-latest-path`; it contains the sidecar, build timing, the
initial 200-sample run, and both paired A/B reports with `/usr/bin/time -lp` output.
