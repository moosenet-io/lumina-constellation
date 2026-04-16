# ✦ Odyssey

> Researching the path ahead.

**Odyssey** is the travel and logistics module that manages itineraries, bookings, and journey planning.

## What it does

- Orchestrates complex multi-leg travel planning and itinerary management.
- Monitors flight statuses, hotel bookings, and transit schedules.
- Integrates with external travel APIs to provide real-time updates.
- Stores traveler preferences and recurring loyalty information.
- Provides logistics context to the morning briefing when trips are active.

## Key files

| File | Purpose |
|------|---------|
| `odyssey.py` | Main travel and logistics orchestration |
| `README.md` | This documentation |

## Talks to

- **[Vigil](../vigil/)** — Adds travel alerts and countdowns to the morning briefing.
- **[Synapse](../synapse/)** — Sends urgent flight change or transit alerts.
- **[Engram](../engram/)** — Stores itineraries and traveler preferences.

## Configuration

API keys for travel providers and calendar integration required in the environment.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
