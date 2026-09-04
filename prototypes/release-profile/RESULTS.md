# Release profile experiment — 2026-09-05

## Decision

Reject the combined release profile. Keep Cargo's default release code generation while the
application is an alpha whose crash reports and local debugging still need symbols.

## Candidate

The isolated branch tested:

```toml
[profile.release]
lto = "thin"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

A `profiling` profile inherited the optimized settings but retained debug information.

## Measurement

Both binaries ran the production headless Search v2 benchmark against the same 5,668,822-Item
base. Each process executed ten cases with 100 randomized samples, one warm-up, and seed 9010.

- benchmark time: 51.60 seconds baseline, 52.36 seconds candidate, 1.48% slower;
- retired instructions: 2.503T baseline, 2.483T candidate, 0.78% fewer;
- CPU cycles: 600.63B baseline, 630.17B candidate, 4.92% more;
- peak physical footprint: 67.78 MiB baseline, 67.60 MiB candidate;
- executable size: 12,080,064 bytes baseline, 6,121,568 bytes candidate, 49.33% smaller;
- final hits remained identical, every case had one top-10 fingerprint, and no target missed.

Per-case p95 moved in both directions. The candidate improved `implicit-path` total p95 by 5.2%,
but regressed `gapped-path` by 7.4% and `direct-multi` by 6.9%. The full candidate build took
2 minutes 36 seconds because LTO changed code generation for the complete dependency graph.

The smaller executable does not justify slower aggregate search, mixed tail behavior, longer
iteration time, and stripped alpha diagnostics. PGO remains a separate future experiment after a
stable representative workload exists.
