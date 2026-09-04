# Fuzzy name-quality reuse experiment

Status: rejected on 2026-09-05. Keep this branch as measured evidence; do not merge the code.

## Hypothesis

Ordinary multi-token matching and Path Interpretation both compute fuzzy quality for Item-name
tokens. The candidate implementation stores one `Option<MatchQuality>` per token in a reusable
worker vector, then lets both interpretations read those values.

## Result

The production corpus showed no repeatable reduction in work:

- seed 9010 suite: 30.60 s baseline, 31.20 s candidate;
- seed 9011 suite: 31.30 s baseline, 30.60 s candidate;
- retired instructions changed by only -0.16% in both pairs;
- cycles changed by +3.0% and -4.6%, so elapsed movement followed scheduler noise;
- individual p95 values were mixed, including direct-multi +12.8% in the first process and
  gapped-path +5.3% in the repeat;
- final hits, top-10 fingerprints, and zero-miss counts remained identical.

Writing every token result into a vector costs about as much as recomputing the one final-name
comparison shared with Path Interpretation. The added state and branches are not justified.

Raw reports and `/usr/bin/time -lp` output are in `/private/tmp` under stems
`beeline-fuzzy-name-quality-reuse-20260905` and
`beeline-fuzzy-name-quality-reuse-repeat-20260905`.
