# ✦ System

> Core service management and host integration.

**System** manages the underlying OS-level integrations and shared services for the fleet.

## What it does

- Manages the local homepage and portal for household users.
- Coordinates shared assets and static files across multiple agents.
- Provides host-level service templates (systemd) for non-Docker tasks.
- Manages system-wide timers and scheduled jobs.
- Provides a unified gateway for external network access.

## Key files

| File | Purpose |
|------|---------|
| `homepage/` | Source for the constellation's local landing page |
| `README.md` | This documentation |

## Talks to

- **[Soma](../soma/)** — Links to the mission control dashboard.
- **[Vigil](../vigil/)** — Feeds data into the briefing display templates.
- **[Synapse](../synapse/)** — Triggers system-level notifications.

## Configuration

Gateway and portal settings configured in the environment.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
