# Myelin — Token Governance

Myelin tracks inference cost across all Lumina agents. It monitors token usage, detects cost runaway, and reports daily summaries. It observes and advises — it never silently blocks inference.

**Deploys to:** <fleet-host> at `/opt/lumina-fleet/myelin/`
**Trigger:** Passive (hooks into LiteLLM usage logs) + daily report cron
**Inference cost:** $0 (pure Python — all cost tracking is math)

## Core Principle

> Observe and advise, never block.

Myelin tracks costs but never silently stops inference. If Seer goes over budget running a research report, Myelin flags it to Lumina — Lumina alerts the operator — the operator decides. No silent suppression.

## What Myelin Does

1. **Passive monitoring** — Reads LiteLLM usage logs as they're written
2. **Runaway detection** — Alerts if a single agent exceeds its daily budget threshold
3. **Daily report** — Generates a cost summary HTML report with per-agent breakdown
4. **Budget advisor** — Recommends inference tier adjustments based on usage patterns

## Runaway Detection

Thresholds per agent (configurable in `constellation.yaml`):

| Agent | Daily budget | Runaway threshold |
|-------|-------------|------------------|
| Lumina | $0.50 | $1.00 |
| Vigil | $0.05 | $0.20 |
| Seer | $0.30 | $0.60 |
| Cortex | $0.10 | $0.30 |
| Mr. Wizard | $0.20 | $0.50 |

When a threshold is crossed, Myelin sends a Nexus message to Lumina with `priority: urgent`. Lumina decides whether to continue, pause, or alert the operator.

## Daily Cost Report

Generated at midnight, written to `/opt/lumina-fleet/reports/myelin/`:

```
┌──────────────────────────────────────────┐
│ Myelin Cost Report — 2026-04-13          │
├──────────┬──────────┬────────┬───────────┤
│ Agent    │ Tokens   │ Cost   │ vs Budget │
├──────────┼──────────┼────────┼───────────┤
│ Lumina   │ 42,100   │ $0.18  │ 36%       │
│ Vigil    │  8,400   │ $0.01  │ 20%       │
│ Seer     │ 71,200   │ $0.22  │ 73%       │
│ Cortex   │  9,600   │ $0.04  │ 40%       │
├──────────┼──────────┼────────┼───────────┤
│ TOTAL    │ 131,300  │ $0.45  │ Under $1  │
└──────────┴──────────┴────────┴───────────┘
```

## MCP Tools (in Terminus)

| Tool | Description |
|------|-------------|
| `myelin_usage(agent, period)` | Get token/cost usage for an agent |
| `myelin_cost_report()` | Generate full daily cost report |
| `myelin_runaway_check()` | Check if any agent is over threshold |

## Related

- [Inference De-Bloating](../architecture/inference-de-bloating.md) — The cost philosophy Myelin enforces
- [Architecture Overview](../architecture/constellation-overview.md)
- LiteLLM: [github.com/BerriAI/litellm](https://github.com/BerriAI/litellm)
