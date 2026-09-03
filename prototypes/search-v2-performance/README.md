# Search v2 performance baseline

## Production headless benchmark

The application binary has a benchmark mode that exits before Tauri initialization, so it
cannot create or activate a window. Unlike the original prototype below, this path calls the
current production q-gram retrieval, matcher, Visit Journal, aliases, Working Set builder, and
ranker directly. Samples from all cases are shuffled by a reproducible seed.

Build once, then run independent sessions from the repository root:

```sh
cargo build --release --manifest-path src-tauri/Cargo.toml

/usr/bin/time -lp src-tauri/target/release/beeline --benchmark-search-v2 \
  --index "/Users/kiri110k/Library/Application Support/com.kiri110k.beeline/name_index/home.idx" \
  --qgram "/Users/kiri110k/Library/Application Support/com.kiri110k.beeline/name_index/home.qgram" \
  --root /Users/kiri110k \
  --app-data "/Users/kiri110k/Library/Application Support/com.kiri110k.beeline" \
  --current /Users/kiri110k/work/wip \
  --cases prototypes/search-v2-performance/queries.tsv \
  --samples 200 \
  --warmups 1 \
  --seed 1101 \
  --session warm-1 \
  --state base > report.json
```

Use a different seed and session name for each independent process. `--state reconciled` first
runs the production diff-rescan into an in-memory overlay; it never persists or changes the live
application's index. The report contains every observation plus p50/p90/p95/p99/max aggregates,
target ranks, top-10 fingerprints, candidate counts, and phase timings. `/usr/bin/time -lp`
supplies process memory and page-fault counters on macOS.

The only missing boundary is Tauri IPC → React commit → paint. Measure that in a separately
scheduled visible-app pass; a hidden WebView can be throttled and is not a valid paint benchmark.

## Historical prototype

Read-only vertical slice for issue #54. It maps the production Name Index v4 and measures
staged Working Set and whole-index retrieval with exact matching, Keyboard Layout Correction,
bounded Typo Correction, Path Interpretation, rank-aware top-k, and a small grouped ranker.
The global stage first runs the production exact/layout matcher as an arrival-time baseline. A
fuzzy supplement follows and the prototype ranker merges both candidate sets.

The crate does not write the production index or application state. Its first baseline uses the
existing 64-bit name filter with a correction-safe relaxation, then verifies candidates with
bounded optimal-string-alignment distance. Alternative q-gram and FST indexes are built only if
this baseline fails measured latency, memory, or result quality.

The mmap baseline failed global fuzzy latency, so the harness also contains the next
sequential candidate: a mapped hashed-trigram sidecar. Pass `--qgram-index`; the harness builds
the file if it does not exist and reuses it on later runs.

Run from the repository root:

```sh
cargo run --release --manifest-path prototypes/search-v2-performance/Cargo.toml -- \
  --index "/Users/kiri110k/Library/Application Support/com.kiri110k.beeline/name_index/home.idx" \
  --root /Users/kiri110k \
  --current /Users/kiri110k/work/wip \
  --recents "/Users/kiri110k/Library/Application Support/com.kiri110k.beeline/recents_cache.json" \
  --journal "/Users/kiri110k/Library/Application Support/com.kiri110k.beeline/visit_journal.ndjson" \
  --pinned "/Users/kiri110k/Library/Application Support/com.kiri110k.beeline/pinned_tabs.json" \
  --cases prototypes/search-v2-performance/queries.tsv \
  --samples 10
```

Add the q-gram stage to that command with:

```sh
  --qgram-index /tmp/beeline-search-v2-qgram-v1.idx
```

The JSON report goes to stdout. Use `/usr/bin/time -l` around the command for process peak RSS.
Measured results and the architecture decision are in `RESULTS.md`.

## Deliberate limits

- This is a retrieval and core-ranking baseline, not the final Search v2 implementation.
- The first run stops at the Rust boundary. It does not claim IPC or rendered UI latency.
- Search Memory uses no real learned state yet. The ranker shape is present, while later traces
  will supply the real distributions needed to tune weights.
- Ordinary multi-token matching requires at least one token in the Item name. Other tokens may
  match ancestor components. Path Interpretation requires its final token to match the Item name.
- Global Typo Correction runs only at the configured five-significant-character threshold.
  Working Set Typo Correction is active from the first character.
