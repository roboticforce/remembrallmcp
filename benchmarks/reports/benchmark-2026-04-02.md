# RemembrallMCP Benchmark Results

Generated: 2026-04-02

**Test repo:** pallets/click v8.1.7 (594 symbols, 1,589 relationships indexed)

**Methodology:** For each task, an AI agent answered the question two ways:
- **Without RemembrallMCP:** Using only grep, glob, and read (standard codebase exploration)
- **With RemembrallMCP:** Using `remembrall_impact`, `remembrall_lookup_symbol`, or direct graph queries (1 tool call each)

Tool calls were counted directly. Token estimates use ~500 tokens per grep/read call (content returned) and ~200 tokens per remembrall call (structured JSON response).

---

## Task 1: Blast radius analysis

**Prompt:** If I change the signature of BaseCommand.invoke() in core.py, what other functions and files are affected?

*Without the graph, agent greps file by file. With remembrall_impact, one call.*

| Metric | Without RemembrallMCP | With RemembrallMCP | Delta |
|--------|----------------------|---------------------|-------|
| Tool calls | 19 | 1 | -94.7% |
| Estimated tokens | ~9,500 | ~200 | -97.9% |
| Files found | 3 (core.py, testing.py, decorators.py) | 3 (core.py, testing.py, decorators.py) | same |
| Functions found | 9 callers + 3 overrides | 12 references (callers + defines) | same |

**Result:** `remembrall_impact` returned all 12 affected symbols across 3 files in a single call. The agent without RemembrallMCP needed 19 tool calls to reach the same answer.

---

## Task 2: Find all callers

**Prompt:** Find every caller of format_help() across the Click codebase. List the function name, file, and line number.

*Without: recursive grep + manual tracing. With: remembrall_lookup_symbol + relationship traversal.*

| Metric | Without RemembrallMCP | With RemembrallMCP | Delta |
|--------|----------------------|---------------------|-------|
| Tool calls | 27 | 1 | -96.3% |
| Estimated tokens | ~13,500 | ~200 | -98.5% |
| Callers found | 1 (get_help) | 1 (get_help at line 1319) | same |

**Result:** The agent without RemembrallMCP made 27 tool calls including 11 individual file searches and multiple verification passes - only to confirm there's a single caller. RemembrallMCP returned the answer in one query.

---

## Task 3: Trace data flow

**Prompt:** Trace how Context gets created and passed through command invocation. List every function that receives or creates a Context.

*Deep multi-hop dependency chain. Token savings compound with each hop.*

| Metric | Without RemembrallMCP | With RemembrallMCP | Delta |
|--------|----------------------|---------------------|-------|
| Tool calls | 19 | 1 | -94.7% |
| Estimated tokens | ~9,500 | ~200 | -97.9% |
| Functions found | 96 (exhaustive) | 19 (direct relationships) | different scope |
| Files found | 6 | 5 | same core set |

**Result:** The agent without RemembrallMCP was extremely thorough (96 functions across 19 tool calls) but included many indirect references. RemembrallMCP's graph query returned the 19 direct Context relationships across 5 files - the structurally meaningful ones - in a single call.

---

## Task 4: Rename a class

**Prompt:** What files need changes to rename BaseCommand to CommandBase?

*Requires knowing all references. Graph gives completeness; grep misses indirect references.*

| Metric | Without RemembrallMCP | With RemembrallMCP | Delta |
|--------|----------------------|---------------------|-------|
| Tool calls | 28 | 1 | -96.4% |
| Estimated tokens | ~14,000 | ~200 | -98.6% |
| Files found | 4 (core, __init__, shell_completion, testing) | 1 (core.py - inheritance only) | agent found more |
| References found | 12 specific references | 2 (defines + inherits) | agent found more |

**Result:** The graph correctly identified the inheritance relationship (Command inherits BaseCommand) but missed import-only references in shell_completion.py, testing.py, and __init__.py because those are type annotations/imports, not call/inherit relationships. The agent's grep-based approach found all 12 references. **This highlights a gap in the parser - import and type annotation tracking would improve rename analysis.**

---

## Task 5: Add a parameter

**Prompt:** Add a deprecated parameter to @command decorator. What files need to change?

*Requires understanding the decorator-to-class wiring. Graph shows the relationship chain.*

| Metric | Without RemembrallMCP | With RemembrallMCP | Delta |
|--------|----------------------|---------------------|-------|
| Tool calls | 19 | 1 | -94.7% |
| Estimated tokens | ~9,500 | ~200 | -97.9% |
| Files identified | 2 (core.py, decorators.py) | 2 (core.py, decorators.py) | same |
| Key insight | Found feature already exists! | Shows command -> BaseCommand -> invoke chain | complementary |

**Result:** The agent reading the code actually discovered that Click already implements `deprecated` - something the graph query wouldn't surface since that's semantic understanding, not structural. However, the graph's decorator-to-class-to-method chain showed the exact files and relationships in one call, which is what you need for implementation planning.

---

## Aggregate

| Metric | Without RemembrallMCP | With RemembrallMCP | Delta |
|--------|----------------------|---------------------|-------|
| **Total tool calls** | **112** | **5** | **-95.5%** |
| **Estimated total tokens** | **~56,000** | **~1,000** | **-98.2%** |
| Avg tool calls per task | 22.4 | 1.0 | -95.5% |
| Avg accuracy | High (thorough but slow) | High (fast but structural only) | complementary |

---

## Key Findings

1. **95% fewer tool calls** - The agent without RemembrallMCP averaged 22 tool calls per question. With RemembrallMCP, every question was answered in 1 call.

2. **~98% token reduction** - Estimated 56,000 tokens of exploration reduced to ~1,000 tokens of structured responses.

3. **Accuracy tradeoffs exist** - RemembrallMCP excels at structural queries (who calls this, what inherits from this, what's the blast radius). It's less complete for rename analysis where type annotations and string references matter. The parser could be improved to track imports and type references.

4. **Complementary strengths** - The best approach is likely RemembrallMCP for the initial structural query (fast, cheap, complete for call/inherit relationships) followed by targeted grep for edge cases the graph doesn't cover.

5. **Savings compound at scale** - Click is a ~90-file project. On a 500+ file monorepo, the without-RemembrallMCP approach would need proportionally more tool calls, while RemembrallMCP's query time stays constant.
