# Staged retrieval threshold prototype

This prototype measures when Search v2 should add the whole Name Index to the always-searched
Working Set. It imports the production v4 reader and ranker, opens the persisted index
read-only, and does not modify Beeline settings or application data.

The Working Set fixture uses the complete direct contents of one current Location plus the
real cached Recents, Visit Journal paths, and Pinned Anchors. Search Memory does not exist in
production yet, so no learned Items are available for this measurement. Run the prototype
with more than one current Location to cover different Working Set shapes.

Build and run on the reference Mac:

```sh
cargo build --release \
  --manifest-path prototypes/staged-retrieval-threshold/Cargo.toml

prototypes/staged-retrieval-threshold/target/release/beeline-staged-retrieval-threshold \
  --index "$HOME/Library/Application Support/com.kiri110k.beeline/name_index/home.idx" \
  --root "$HOME" \
  --current "$HOME/work" \
  --recents "$HOME/Library/Application Support/com.kiri110k.beeline/recents_cache.json" \
  --journal "$HOME/Library/Application Support/com.kiri110k.beeline/visit_journal.ndjson" \
  --pinned "$HOME/Library/Application Support/com.kiri110k.beeline/pinned_tabs.json" \
  --cases prototypes/staged-retrieval-threshold/queries.tsv \
  --alias "bee=$HOME/lab/beeline"
```

The `bee` alias is synthetic because the real Alias Dictionary was empty during the recorded
run. It verifies that an exact alias injects its target without waiting for the global
threshold. The report also asserts that changing the threshold around an unchanged active
five-character query immediately changes its scope.

The prototype compares scope and current production candidate shapes. It does not choose the
future global fuzzy index, implement Search Memory, or modify the production ranker. Those
belong to their own Search v2 tickets.
