# ✦ Vigil

> Good morning. Here's what you need to know.

**Vigil** is the morning briefing agent that compiles data from across the constellation into a daily report.

## What it does

- Aggregates weather, calendar, and traffic data every morning.
- Summarizes system health reports from Sentinel.
- Provides a financial summary including Myelin's budget tracking.
- Delivers the briefing via Matrix or email at 7:00 AM local time.
- Maintains a historical archive of daily briefings.

## Key files

| File | Purpose |
|------|---------|
| `briefing.py` | Briefing generation and delivery logic |
| `briefing_dashboard.py` | Web-based view for the daily briefing |
| `council_prioritize.py` | Ranks news and events by relevance |

## Talks to

- **[Sentinel](../sentinel/)** — Retrieves infrastructure health checks.
- **[Myelin](../myelin/)** — Gets daily budget and token usage stats.
- **[Synapse](../synapse/)** — Routes the final briefing to the operator.

## Configuration

Scheduled via `scheduler.py` in the fleet root. Delivery targets configured in `synapse`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
