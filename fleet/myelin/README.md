# ✦ Myelin

> Counts every token so you don't have to.

**Myelin** is the cost governance module that monitors and enforces the constellation's inference budget.

## What it does

- Tracks token usage across local Ollama and cloud providers (OpenRouter, Anthropic).
- Manages per-consumer virtual keys to attribute costs to specific agents.
- Enforces daily and weekly spend limits with a hardware circuit breaker.
- Generates burn rate forecasts and weekly financial reports.
- Optimizes model routing based on current budget and task complexity.

## Key files

| File | Purpose |
|------|---------|
| `myelin_collect.py` | Usage data collection from LiteLLM and logs |
| `myelin_burn_planner.py` | Budget forecasting and limit enforcement |
| `myelin_weekly_report.py` | Generates summaries for the Vigil briefing |
| `myelin_config.yaml` | Spend limits and provider pricing data |

## Talks to

- **[Vigil](../vigil/)** — Feeds financial summaries into the morning briefing.
- **[Sentinel](../sentinel/)** — Triggers emergency shutdowns on cost runaways.
- **[Terminus](../../terminus/)** — Provides usage metrics to the mission control tools.

## Configuration

Budget limits set in `myelin_config.yaml`. Requires access to the LiteLLM/OpenRouter log database.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
