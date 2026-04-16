# ✦ Sentinel

> 20 checks. 30 minutes. Zero tolerance.

**Sentinel** is the infrastructure monitoring service that ensures all constellation modules are healthy.

## What it does

- Performs recurring health checks on all Docker containers and services.
- Monitors LLM token usage to prevent runaway costs (Runaway Guard).
- Generates a real-time status page for the Soma dashboard.
- Exports metrics to Prometheus for long-term observability.
- Triggers remediation workflows via Council when issues are detected.

## Key files

| File | Purpose |
|------|---------|
| `ops.py` | Core monitoring and alerting logic |
| `health_checks.py` | Service-specific validation routines |
| `llm_runaway_guard.py` | Kills sessions exceeding cost thresholds |
| `status_generator.py` | Produces the `status.json` for the dashboard |

## Talks to

- **[Soma](../soma/)** — Feeds status data to the mission control dashboard.
- **[Synapse](../synapse/)** — Sends critical alerts to the operator.
- **[Myelin](../myelin/)** — Checks current spend against the safety buffer.

## Configuration

Alert thresholds defined in `alert_rules.py`. Scans the local Docker socket by default.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
