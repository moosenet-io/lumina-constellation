# ✦ Household

> Mission control for your home.

**Household** is the coordination layer for domestic tasks, partner integration, and home automation.

## What it does

- Manages partner onboarding and identity integration for the constellation.
- Coordinates shared household tasks and reminders.
- Provides a bridge for household-specific bots and automation scripts.
- Maintains the "Partner Runbook" for constellation shared access.
- Tracks household-level preferences and routines.

## Key files

| File | Purpose |
|------|---------|
| `partner-onboarding-runbook.md` | Guide for adding household members |
| `bridge_bot.py` | Integration bridge for household chat services |
| `partner_identity.md` | Persona definitions for household partners |

## Talks to

- **[Nexus](../nexus/)** — Receives and routes household requests from the inbox.
- **[Synapse](../synapse/)** — Delivers household reminders and alerts.
- **[Soma](../soma/)** — Displays household status on the dashboard.

## Configuration

Partner identities and onboarding rules defined in the respective markdown files.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
