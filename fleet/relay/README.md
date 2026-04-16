# ✦ Relay

> Connecting the household.

**Relay** is the vehicle and external asset integration module for Lumina Constellation.

## What it does

- Monitors vehicle status, charging levels, and maintenance alerts.
- Integrates with smart home and IoT devices for household automation.
- Provides external asset data to the Sentinel monitoring service.
- Manages remote access and control for supported external hardware.
- Logs usage patterns and efficiency metrics for long-term analysis.

## Key files

| File | Purpose |
|------|---------|
| `relay.py` | Main asset integration and monitoring logic |
| `README.md` | This documentation |

## Talks to

- **[Sentinel](../sentinel/)** — Reports on the health and status of external assets.
- **[Vigil](../vigil/)** — Provides vehicle range and "ready-to-go" status for briefings.
- **[Soma](../soma/)** — Displays asset status on the mission control dashboard.

## Configuration

Requires integration-specific credentials (e.g., Tesla API, Home Assistant) in the environment.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
