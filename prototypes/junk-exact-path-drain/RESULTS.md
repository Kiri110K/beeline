# Deferred exact-path Junk drain experiment

Status: rejected on 2026-09-05. Keep the branch as measured evidence; do not merge the code.

## Trigger

The installed hidden app drained 499 dirty Junk directories in 15,047 ms after a build burst. The
worker intentionally runs at background QoS and only drains after quiet on external power, but the
long wall time made an exact-path alternative worth measuring.

## Candidate

The watcher retained up to 100,000 unique Junk event paths. A quiet drain applied sparse parents
path by path and used one shallow directory reconcile above 256 paths per parent. Saturation fell
back to the existing dirty-directory set. The overlay journal would reconstruct the same pending
paths after a crash.

## Result

One existing file inside the 90,000-child `target/debug/deps` directory drained in 0.023–0.058 ms;
the old full-parent reconcile took 160–172 ms. That isolated case did not represent the real burst.

Two release matrices replayed the current production overlay journal:

- 28,867 source paths: 517 directory fallbacks took 1,188 ms; hybrid 18,247 exact paths plus five
  fallbacks took 1,138 ms;
- 28,919 source paths: directory mode took 616 ms; hybrid 18,287 exact paths plus five fallbacks
  took 1,130 ms.

Both modes had the same 73 disk/index mismatches among explicit journal paths. Their total Item
counts differed by about 3,200 because directory reconciliation also discovers unlisted siblings;
the narrower exact path set does not preserve that useful repair behavior. On the representative
mix, hybrid work is not consistently faster and adds several MiB of bounded path state plus a second
maintenance algorithm. Keep the current directory drain. Its 15-second installed-app wall time is
mainly background-QoS throttling rather than equivalent active CPU time, and it runs only after a
quiet window on external power or synchronously for a Junk-targeting query.
