# 005 · [M] Reproducible retrieval eval harness

**Priority:** M · **Status:** pending

## Context
Mem0 competes with published benchmark numbers (LoCoMo, LongMemEval).
Without a reproducible eval, quality claims — and tuning decisions like
RRF k, per-signal top-N, recency weights — are guesswork.

## Scope
- `evals/` folder with: a seed dataset (~100 memories across 3-4 fake
  projects + ~50 queries with expected memory ids, including lexical-only,
  semantic-only, diacritics, as_of and global-scope cases), and a runner
  binary (`cargo run --bin eval` or `examples/eval.rs`) that reports
  hit@k (k=1,3,8) and MRR against a chosen embedder.
- Wire the runner into CI with MockEmbedder (deterministic floor); real
  Ollama runs stay manual/local.
- Document baseline numbers in `evals/RESULTS.md` so regressions show up
  in diffs.

## Technical details
- Reuse `providers::mock::MockEmbedder` pinning for deterministic CI runs;
  a `--provider ollama` flag switches to real embeddings locally.
- Keep dataset in JSON fixtures; loader seeds an in-memory DB via
  `db::queries` (never bypass the query layer).
- LongMemEval subset import can come later; start with the owned dataset.

## Acceptance criteria
- [ ] `eval` runs green in CI with mock embeddings and prints hit@k / MRR.
- [ ] RESULTS.md has a committed baseline; changing RRF constants shows a
      measurable delta locally.

## Out of scope
End-to-end extraction quality evals (LLM-judged); latency benchmarks.
