# Name Index v4 storage prototype

This is the storage experiment that selected the production v4 design. Production now uses
the mapped-base and mutable-overlay model in `src-tauri/src/name_index/`; the prototype stays
as reproducible evidence for the original heap-versus-mmap decision.

This prototype compares two ways to hold the same compact, immutable Name Index image:

- `heap`: read the complete image into one `Vec<u8>`;
- `mmap`: map the image read-only and let macOS page it.

The prototype itself does not change Beeline's production index. The generated image belongs
in a temporary directory, not in the repository.

The candidate v4 image uses one UTF-8 name arena, 20-byte directory records, and 24-byte
Item records. Conversion removes directory nodes that are no longer reachable from the
root. Both readers run the same candidate-set scan over the standard live queries and
print a deterministic hash, so their results can be compared directly.

Build and run:

```sh
rustc -O prototypes/name-index-v4/main.rs -o /private/tmp/beeline-name-index-v4-prototype
/private/tmp/beeline-name-index-v4-prototype build \
  "$HOME/Library/Application Support/com.kiri110k.beeline/name_index/home.idx" \
  /private/tmp/beeline-name-index-v4-prototype.idx
/usr/bin/time -l /private/tmp/beeline-name-index-v4-prototype bench-heap \
  /private/tmp/beeline-name-index-v4-prototype.idx
/usr/bin/time -l /private/tmp/beeline-name-index-v4-prototype bench-mmap \
  /private/tmp/beeline-name-index-v4-prototype.idx
/private/tmp/beeline-name-index-v4-prototype bench-overlay \
  /private/tmp/beeline-name-index-v4-prototype.idx
```

The scan checks name matching, wrong-layout correction, and multi-token ancestor matching.
It does not implement production ranking, Visit Journal boosts, aliases, or result sorting.
Lossless record conversion plus identical heap/mmap candidate hashes make this useful for
choosing storage, but production integration still needs golden ranked-result tests through
the real ranker.

`bench-overlay` adds a tombstone bitset and owned overlay records to the mapped base. It
checks 10,000 removals, 5,000 renames, and 5,000 additions without writing the image.
