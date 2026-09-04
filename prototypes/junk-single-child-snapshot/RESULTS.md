# Junk single-child-snapshot experiment

Status: rejected on 2026-09-05. Keep the branch as measured evidence; do not merge the code.

The candidate reused the first indexed-child snapshot while reconciling a dirty Junk directory,
instead of calling `direct_children` again after removals. It is semantically equivalent but does
not address the measured cost.

Five release runs against the production index and the 90,000-entry
`src-tauri/target/debug/deps` directory measured:

- baseline: 172.18, 160.14, 167.96, 166.38, and 168.78 ms;
- candidate: 167.92, 150.56, 164.70, 166.41, and 170.82 ms.

The ranges overlap and the mean moved by less than 2%. Directory enumeration and creation of the
disk/index snapshots dominate. The next experiment should avoid rescanning every sibling when the
watcher already supplied exact changed paths.
