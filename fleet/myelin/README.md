# ✦ Myelin

> "Counts every token so you don't have to."

**Myelin** is Lumina's inference cost governance module — per-consumer budgets, burn rate tracking, subscription pacing, and the circuit breaker that saves you from runaway cloud spend.

## What it does

- **Virtual key management** (MY.1–MY.9): each consumer (IronClaw, Vector, Vigil, etc.) gets its own LiteLLM key with a daily budget and RPM limit
- **Burn rate tracking**: queries LiteLLM `/spend/logs` and aggregates per-consumer spend
- **Weekly cost reports**: Monday morning HTML report with 7-day breakdown by consumer and model
- **Budget alerts**: fires alerts via Nexus when consumers approach or hit limits
- **Circuit breaker**: global $10/day autonomous spend cap — distinguishes operator vs agent usage

## Key files

| File | Purpose |
|------|---------|
| `myelin_collect.py` | Polls LiteLLM for spend data, stores to local DB |
| `myelin_alerts.py` | Evaluates spend against thresholds, sends alerts via Nexus |
| `myelin_burn_planner.py` | Daily budget pacing — suggests throttle if burn rate is high |
| `myelin_weekly_report.py` | Generates HTML weekly cost summary |
| `myelin_config.yaml` | Consumer registry, budget thresholds, alert settings |

## Talks to

- **LiteLLM** (`LITELLM_URL`) — source of all spend data via `/spend/logs`
- **Nexus** — routes budget alerts to Lumina
- **Sentinel** — feeds into Prometheus metrics (`lumina_inference_cost_daily_usd`)
- **Soma** — cost dashboard data source

## Configuration

```bash
LITELLM_URL=http://your-litellm-host:4000
LITELLM_MASTER_KEY=sk-...       # admin key for /spend/logs queries
INBOX_DB_HOST=your-postgres-host
```

Consumer virtual keys (MY.1–MY.9) generated via `fleet/security/generate_litellm_keys.py`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
