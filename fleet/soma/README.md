# ✦ Soma

> Mission control for your household AI.

**Soma** is the central dashboard and administrative interface for Lumina Constellation.

## What it does

- Provides a unified web interface for all constellation modules.
- Visualizes system health, budget burn, and recent agent activity.
- Manages agent skills and proposes new capabilities based on usage.
- Hosts the "Soma Wiki" for internal documentation and runbooks.
- Authenticates and secures access to the constellation control plane.

## Key files

| File | Purpose |
|------|---------|
| `api/` | Flask-based backend for the dashboard |
| `templates/` | Jinja2 templates for the web UI |
| `auth.py` | Session management and security |
| `soma_review.py` | Interface for reviewing agent decisions |

## Talks to

- **[Sentinel](../sentinel/)** — Displays real-time infrastructure status.
- **[Myelin](../myelin/)** — Renders budget and token usage charts.
- **[Engram](../engram/)** — Queries memory for the activity feed.

## Configuration

Runs on port 8082 by default. Configure `SECRET_KEY` in the environment for session security.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
