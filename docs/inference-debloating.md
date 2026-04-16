# Inference de-bloating

> The philosophy that keeps Lumina at $0/day.

## The problem

Most AI systems send everything to an LLM. Weather check? LLM. Timer formatting? LLM. Health check ping? LLM. This burns tokens on tasks that don't need intelligence.

## The solution

Three tiers, strict routing:

| Tier | Handles | Cost | Example |
|------|---------|------|---------|
| **Python** (~90%) | Deterministic tasks | $0 | Fetch weather API, format timestamps, check disk space, parse JSON |
| **Local Qwen** (~8%) | Tasks needing language understanding | $0 | Summarize a page, classify a request, generate a briefing paragraph |
| **Cloud AI** (~2%) | Tasks needing frontier reasoning | $/token | Obsidian Circle deliberation, complex code review, novel problem solving |

## How it works in practice

When Vigil builds the morning briefing:
- **Python** fetches weather, traffic, calendar, budget → $0
- **Python** formats everything into the briefing template → $0
- **Local Qwen 9B** writes the natural language summary → $0
- **Cloud AI** is never called — the briefing doesn't need frontier reasoning

Total cost: $0.00. Every day. Forever.

## The decision chain

Before any LLM call, ask in order:

1. Can Python handle this? → **Python ($0)**
2. Can a template + variables handle this? → **Template ($0)**
3. Can a keyword lookup handle this? → **Lookup table ($0)**
4. Does it need NL parsing that regex can't do? → **Local Qwen ($0)**
5. Does it need synthesis beyond templates? → **Local Qwen ($0)**
6. Does it need multi-source synthesis? → **Haiku (~$0.001)**
7. Does it need complex reasoning? → **Sonnet (~$0.01-0.05)**
8. Is this a critical architectural decision? → **Opus (gated)**

Stop at the first YES. Most requests stop at step 1 or 2.

## The economic principle

> "Inference is a resource to be allocated, not a constant."

Myelin tracks per-consumer spend with virtual keys (MY.1-9). Each agent has a daily budget. Sentinel fires circuit breakers at $10/day for autonomous spend. Operator sessions (Terminal 1 OAuth, Terminal 2 build) are tracked but not circuit-broken.

If you have 64GB+ of unified memory, your daily inference cost should be **$0.00**. Cloud is opt-in for specific tasks, never the default path.
