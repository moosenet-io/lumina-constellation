# ✦ Shared

> Common utilities and templates for the fleet.

**Shared** contains the libraries, templates, and utilities used across all Lumina agents.

## What it does

- Houses common Jinja2 templates for briefings, alerts, and dashboards.
- Provides shared Python utilities for logging, config loading, and PII scrubbing.
- Maintains consistent UI components for web-based modules.
- Defines shared data schemas and message formats.
- Provides base classes and interfaces for specialized agents.

## Key files

| File | Purpose |
|------|---------|
| `templates/` | Shared Jinja2 templates for all services |
| `README.md` | This documentation |

## Talks to

- **[Vigil](../vigil/)** — Supplies templates for the morning briefing.
- **[Synapse](../synapse/)** — Provides formatting for outgoing notifications.
- **[Soma](../soma/)** — Shared UI components for the dashboard.

## Configuration

Individual components may have their own configuration, but many are driven by fleet-wide env vars.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
