# RemembrallMCP Benchmark Roadmap

*Last updated: April 2026*

## Goal

Benchmark the product surface RemembrallMCP actually exposes:

1. Persistent memory retrieval quality
2. Long-horizon conversational memory behavior
3. Code graph and impact-analysis correctness
4. End-to-end agent productivity on real coding tasks

No single public benchmark covers all four. The benchmark strategy should therefore be a layered evaluation stack, not a search for one headline number.

## Product Surface to Benchmark

| Surface | User-facing claim | Primary tools |
|---|---|---|
| Memory retrieval | Agents can recall relevant prior decisions, incidents, and patterns | `remembrall_recall`, `remembrall_store`, `remembrall_update`, `remembrall_delete` |
| Long-term memory | Agents can handle updates, time, contradictions, and multi-session recall | `remembrall_recall`, `remembrall_store`, `remembrall_update` |
| Code intelligence | Agents can answer "what breaks if I change this?" with high recall | `remembrall_index`, `remembrall_impact`, `remembrall_lookup_symbol` |
| Agent acceleration | Remembrall reduces exploration cost and improves task completion | all tools together |

## Current Coverage

RemembrallMCP already has the beginnings of the right benchmark stack.

### 1. Agent productivity A/B benchmark

- Location: `benchmarks/`
- Measures: tool calls, estimated tokens, wall clock time, task accuracy
- Strength: directly supports the "agents explore less" product claim
- Gap: current task set is small and handcrafted

### 2. Parser and impact correctness harness

- Location: `crates/remembrall-test-harness/`
- Measures: symbol recall/precision, relationship recall/precision, import resolution, impact accuracy, edge cases
- Strength: directly validates the code graph
- Gap: focuses on parser correctness more than full MCP tool behavior

### 3. Memory recall quality harness

- Location: `crates/remembrall-recall-test/`
- Measures: Recall@5, Precision@5, MRR, latency, pass/fail thresholds
- Strength: directly validates retrieval quality
- Gap: limited coverage for temporal updates, stale memories, abstention, and multi-session synthesis

## External Benchmarks: Fit Assessment

The right question is not "what is the industry standard benchmark for memory?" but "which public suites add signal on top of our product-specific harnesses?"

| Benchmark | Best for | Fit | Why |
|---|---|---|---|
| SWE-bench Verified | End-to-end coding task success | High | Best external measure for whether Remembrall helps an agent finish real repo tasks |
| LongMemEval | Long-term conversational memory | High | Strong match for updates, temporal reasoning, multi-session memory, abstention |
| LoCoMo | Long conversation memory and event QA | Medium-high | Good second option for long-horizon memory, broader conversational structure |
| BEIR | Generic IR retrieval quality | Medium | Useful regression signal for retrieval, but not memory-specific and not code-specific |
| RepoBench | Repository-level code retrieval/completion | Medium | Relevant to repo context retrieval, less relevant to impact analysis and persistent memory |
| RAGAS | LLM app evaluation framework | Medium-low | Useful as infrastructure, not as a benchmark target by itself |

## Recommendation

Use a four-layer benchmark stack:

1. **Keep custom evals as primary**
   - They are the only benchmarks aligned to the full Remembrall product surface.
2. **Add SWE-bench Verified for external coding-agent validation**
   - This is the strongest independent check on the productivity claim.
3. **Add one long-memory benchmark**
   - Prefer `LongMemEval`.
   - Use `LoCoMo` only if we want a second conversational-memory view later.
4. **Optionally add a generic retrieval regression suite**
   - Prefer a narrow BEIR slice instead of a large generic benchmark rollout.

## What Each Benchmark Should Prove

### A. Code graph correctness

Question:
"Does the graph return the right symbols and dependency edges?"

Owner:
`crates/remembrall-test-harness`

Success metrics:

- Symbol recall >= 95% on small fixtures
- Relationship recall >= 85% on small fixtures
- Import resolution >= 90%
- Impact query recall >= 95% for direct callers
- No language below grade `B` on supported languages

Release gate:

- Any drop >5 points on a language dimension triggers investigation
- Any impact-analysis regression on `must_find` relationships blocks merge

### B. Memory retrieval quality

Question:
"Does `remembrall_recall` return the right memories quickly?"

Owner:
`crates/remembrall-recall-test`

Success metrics:

- Aggregate Recall@5
- Aggregate Precision@5
- MRR
- P50/P95 latency
- False positive rate for "should abstain" queries
- Filter correctness for `memory_type`, `tags`, and `project`

Release gate:

- No regression in pass rate
- No increase in false positives for abstention queries
- P95 latency stays within a defined tolerance band

### C. Long-horizon memory behavior

Question:
"Does memory remain correct across sessions, updates, conflicting facts, and time?"

Owner:
new harness under `benchmarks/long_memory/` or a dedicated crate if execution logic grows

Preferred benchmark:
`LongMemEval`

What to measure:

- Information extraction
- Multi-session reasoning
- Temporal reasoning
- Knowledge updates
- Abstention

How to adapt it for Remembrall:

- Run the benchmark through a thin adapter that maps session facts into `remembrall_store`
- For update scenarios, explicitly use `remembrall_update` or store superseding memories and verify stale facts stop winning
- Score both retrieval quality and final answer quality

Pass criteria for adoption:

- Benchmark can run reproducibly from the repo
- Results are stable enough across repeated runs to detect regressions
- At least one metric maps directly to Remembrall behavior rather than only model behavior

### D. End-to-end coding-agent productivity

Question:
"Do agents finish real engineering tasks faster, cheaper, or more accurately with Remembrall?"

Owner:
`benchmarks/`

Preferred benchmark:
`SWE-bench Verified` plus current handcrafted A/B tasks

What to measure:

- Task success rate
- Cost proxy: tokens and tool calls
- Latency / wall clock
- Retrieval usage patterns
- Failure modes: missed callers, wasted exploration, bad stale-memory retrieval

Why both are needed:

- Current handcrafted tasks isolate the graph value proposition clearly
- SWE-bench validates that those gains survive in messy, real-world task flows

## Phased Rollout

### Phase 0: Stabilize current benchmark story

Status:
in progress

Work:

- Keep `benchmarks/` as the primary marketing benchmark
- Keep parser and recall harnesses as release-safety benchmarks
- Add this roadmap and align docs around the layered strategy

Exit criteria:

- Benchmark surfaces are documented
- Each existing harness has explicit ownership and intended use

### Phase 1: Expand memory-recall harness for time and updates

Why first:
This is the cheapest high-signal improvement because the harness already exists.

Work:

- Add recall cases for superseded facts
- Add cases for contradictory memories with timestamps
- Add abstention cases where semantically related memories should not be returned
- Add cases that require combining two related memories from different sessions
- Add report fields for false-positive rate and stale-fact rate

Suggested new categories:

- `G`: temporal updates
- `H`: contradictions and supersession
- `I`: abstention
- `J`: multi-memory synthesis

Deliverables:

- expanded `tests/recall/ground_truth.toml`
- scorer updates for stale-fact and abstention failures
- seed fixture additions

### Phase 2: Add LongMemEval-inspired adapter

Why second:
Only after the local harness tests the same failure modes.

Work:

- Vendor or document a pinned LongMemEval version
- Build a runner that converts benchmark sessions into memory writes
- Evaluate two modes:
  - retrieval-only: inspect whether the right memory items are surfaced
  - answering: use a fixed model and prompt to answer benchmark questions from retrieved memories
- Publish a concise result table in `benchmarks/reports/`

Important constraint:

Do not present this as "the" Remembrall benchmark. It covers the long-memory slice, not the code graph.

### Phase 3: Add SWE-bench Verified A/B

Why third:
Highest upside, but operationally heavier.

Work:

- Pick a small pinned subset of SWE-bench Verified tasks first
- Define a reproducible with/without-Remembrall protocol
- Require fresh conversations and pre-indexed repos for the `with` arm
- Record:
  - solve rate
  - time to first correct patch
  - total tool calls
  - total tokens
  - whether Remembrall tools were actually used

Start small:

- 10 to 25 tasks
- 2 or 3 representative repos
- one supported language first, then expand

Success criteria:

- measurable increase in solve rate or equivalent solve rate with materially lower tool cost

### Phase 4: Optional generic retrieval baseline

Why optional:
Useful for engineering discipline, weak for product storytelling.

Work:

- Run a narrow BEIR slice against the memory search layer
- Use it as a regression dashboard, not a homepage claim

## Reporting Model

Keep benchmark reports split by audience.

### Engineering reports

Purpose:
catch regressions and guide implementation work

Include:

- per-category scores
- failure cases
- latency percentiles
- benchmark version and dataset pinning

### Product and marketing reports

Purpose:
support claims without overselling benchmark scope

Include:

- A/B coding-agent savings from `benchmarks/`
- parser quality summary by language
- memory recall summary
- external benchmark slices with tight scope labels

Avoid:

- collapsing everything into one synthetic "benchmark score"
- calling LongMemEval the product benchmark
- claiming broad superiority from retrieval-only results

## Repository Changes to Prioritize

### Near term

1. Expand `remembrall-recall-test` to cover temporal memory behavior
2. Add a machine-readable benchmark manifest with benchmark name, version, scope, and owner
3. Add report templates so results are comparable across runs

### Mid term

1. Add `benchmarks/long_memory/README.md`
2. Add LongMemEval adapter and pinned instructions
3. Add a small SWE-bench Verified subset harness

### Nice to have

1. Trend charts for benchmark regressions over time
2. CI mode for lightweight subsets
3. Benchmarks that isolate the value of `remembrall_update` and stale-memory suppression

## Decision Summary

Use this stack:

- **Primary product benchmarks:** existing custom A/B benchmark, parser harness, recall harness
- **Best external benchmark to add:** SWE-bench Verified
- **Best long-memory benchmark to add:** LongMemEval
- **Optional retrieval regression benchmark:** BEIR slice

That keeps the benchmark program aligned to the actual product instead of drifting toward benchmark theater.
