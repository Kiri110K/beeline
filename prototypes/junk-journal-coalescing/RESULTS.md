# Junk overlay journal coalescing

## Decision

Accept the change. Store one durable logical marker per dirty Junk directory instead of every
changed child path. The marker reproduces the same lazy refresh state while reducing the live
journal by 82% and release replay application time by 68%.

## Why this is safe

Production never applies a Junk child event directly. It marks the child's indexed parent
directory dirty, then reconciles that directory after a quiet window or a Junk-targeting query.
All child events with the same parent therefore carry the same durable information.

The journal represents that information with a synthetic child name containing NUL. macOS file
names cannot contain NUL, so it cannot collide with a user file. Replay recognizes and removes
the marker before any filesystem call, then marks its parent directory directly. This remains
correct if the user changes Junk patterns between recording and replay.

An event for the first Junk component itself stays as its real path because it dirties the
non-Junk parent. Normal and Hidden paths remain unchanged. Journal append still happens before
the in-memory update, preserving the existing crash ordering.

## Production-copy measurement

The ignored machine-local test copied the live journal and mapped the real 5.67-million-Item
base. It applied both the original and coalesced replay to independent copies of that base.

| Metric | Original paths | Coalesced markers |
| --- | ---: | ---: |
| Journal records | 49,838 | 8,983 |
| Journal bytes | 6,951,098 | 1,145,256 |
| Release apply time | 960 ms | 305 ms |
| Dirty directories | 560 | 560 |

The test asserts equal final Item counts and identical dirty-directory sets. The one-time atomic
load rewrite took 287 ms and constructing the replay vector took 2 ms. The migration startup is
therefore about 594 ms for rewrite plus apply, compared with 960 ms for the old replay. Later
starts avoid the rewrite and pay about 307 ms.

The same test under debug took 6.78 seconds for raw paths and 9.46 seconds for markers; optimizer
effects reverse that result. Only the release measurement is relevant to the installed app.

Coalescing also keeps build/cache churn away from the 80,000-entry base-compaction threshold.
That avoids unnecessary base rewrites and their subsequent q-gram rebuild.

Run the production-copy measurement with:

```sh
BEELINE_LIVE_OVERLAY="$HOME/Library/Application Support/com.kiri110k.beeline/name_index/home.overlay.ndjson" \
BEELINE_LIVE_INDEX="$HOME/Library/Application Support/com.kiri110k.beeline/name_index/home.idx" \
BEELINE_LIVE_ROOT=/Users/kiri110k \
cargo test --release --manifest-path src-tauri/Cargo.toml \
  name_index::overlay_journal::tests::live_journal_coalescing_timing \
  --lib -- --ignored --nocapture
```
