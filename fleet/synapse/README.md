# ✦ Synapse

> The right notification, to the right person, at the right time.

**Synapse** is the notification routing and delivery engine for the Lumina Constellation.

## What it does

- Routes alerts and messages from constellation modules to various delivery targets.
- Supports delivery via Matrix, email, webhooks, and local system notifications.
- Manages notification urgency and deduplicates repetitive alerts.
- Provides a "quiet mode" and scheduled delivery windows.
- Maintains a log of all outgoing communication for audit purposes.

## Key files

| File | Purpose |
|------|---------|
| `scanner.py` | Scans for pending messages in the delivery queue |
| `composer.py` | Formats and prepares notifications for specific platforms |
| `gate.py` | Enforces rate limits and delivery rules |
| `synapse.service` | Systemd service for the notification worker |

## Talks to

- **[Vigil](../vigil/)** — Delivers the daily morning briefing to the operator.
- **[Sentinel](../sentinel/)** — Routes critical infrastructure alerts.
- **[Soma](../soma/)** — Displays notification history on the dashboard.

## Configuration

Delivery credentials (Matrix homeserver, SMTP settings) configured in the environment.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
