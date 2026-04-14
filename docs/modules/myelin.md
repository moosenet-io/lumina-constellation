# Myelin — Token Governance

Myelin tracks inference costs across all agents and providers, alerts on budget issues, and provides the Soma cost dashboard.

## What it tracks

| Provider | Measurement | Notes |
|----------|------------|-------|
| OpenRouter | Measured | From API response metadata |
| LiteLLM | Measured | From /spend/logs endpoint |
| Claude Max (OAuth) | Estimated | From session timing + file size |
| Local Qwen | $0 | Always free |

**Important:** Claude Max usage is labeled "estimated" — IronClaw doesn't expose token counts programmatically. Myelin uses session log size as a proxy.

## Alert thresholds

- Agent spending 3x+ daily baseline → urgent Nexus alert
- Daily spend > $3.00 soft limit → normal Nexus alert
- OpenRouter balance < $10 → normal alert
- OpenRouter balance < $2 → urgent alert

## Timers

- `myelin-collect.timer` — every 30 minutes, collects all usage
- `myelin-alerts.timer` — every 30 minutes, checks thresholds

## Dashboard

Gateway `/api/cost` endpoint serves Myelin data to Homepage widget.
