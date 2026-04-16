# ✦ Commute

> Getting there on time, every time.

**Commute** is the traffic and transit intelligence module for Lumina Constellation.

## What it does

- Monitors real-time traffic conditions for configured routes.
- Provides estimated arrival times and "best time to leave" recommendations.
- Integrates with transit APIs to track bus and rail delays.
- Calculates commute baselines to detect unusual congestion.
- Feeds traffic alerts into the morning briefing.

## Key files

| File | Purpose |
|------|---------|
| `commute_check.py` | Real-time traffic monitoring and alerting |
| `commute_baseline.py` | Historical data analysis for route baselines |

## Talks to

- **[Vigil](../vigil/)** — Provides traffic summaries for the morning report.
- **[Synapse](../synapse/)** — Sends urgent transit delay alerts.
- **[Engram](../engram/)** — Stores historical commute data for baseline analysis.

## Configuration

Requires API keys for traffic providers (e.g., TomTom, Google Maps) and configured home/work addresses.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
