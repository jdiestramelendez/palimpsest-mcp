# Retrieval eval — committed baseline

Run with the deterministic mock embedder so results are reproducible in CI:

```sh
cargo run --bin eval             # report
cargo run --bin eval -- --check  # enforce the floors (CI)
cargo run --bin eval -- --provider ollama   # real embeddings, local only
```

## Baseline (2026-08-28, mock provider, defaults: RRF k=60, per-signal top-20, recency 0.25/30d)

```
dataset: 99 memories, 50 queries | provider: mock
category       n   hit@1   hit@3   hit@8     MRR
as_of          2   1.000   1.000   1.000   1.000
current        4   1.000   1.000   1.000   1.000
diacritics     2   1.000   1.000   1.000   1.000
dual           1   1.000   1.000   1.000   1.000
entity         2   1.000   1.000   1.000   1.000
global         3   1.000   1.000   1.000   1.000
lexical       25   0.880   1.000   1.000   0.940
semantic      11   1.000   1.000   1.000   1.000
OVERALL       50   0.940   1.000   1.000   0.970
```

## Floors enforced by --check

- overall hit@8 >= 0.90
- overall MRR   >= 0.70

Floors sit below the baseline on purpose: they catch regressions in the
ranking pipeline (a broken leg, filter or fusion), not dataset noise. When
a deliberate ranking change moves the numbers, update this file in the
same commit and adjust the floors in src/bin/eval.rs if warranted.

## Notes

- The three hit@1 misses in the lexical bucket are hash-embedder noise
  (mock vectors are bag-of-words hashes, so common words correlate);
  they represent honest headroom for ranking-tuning experiments.
- `as_of` queries disable the recency boost and usage tracking by design.
- The mock run exercises the full production path (SQL legs, RRF fusion,
  recency re-rank) — only the embedding vectors are synthetic. Extraction
  quality is out of scope here (see ticket 005 "Out of scope").
