# ✦ Axon

> Work orders in. Results out. No drama.

**Axon** is the work queue executor that polls for tasks and dispatches them to the appropriate agent.

## What it does

- Polls Plane CE for new tasks assigned to the agent fleet.
- Dispatches work orders to Lumina or other specialized agents.
- Tracks task execution status and updates the project management layer.
- Integrates with Pulse for real-time activity streaming.
- Maintains execution context using Engram memory.

## Key files

| File | Purpose |
|------|---------|
| `axon.py` | Main work queue polling and dispatch logic |
| `README.md` | This documentation |

## Talks to

- **[Plexus (Plane)](../nexus/)** — Polls for new work orders and tasks.
- **[Engram](../engram/)** — Retrieves and stores task-related memory.
- **[Pulse](../terminus/)** — Streams execution status and activity logs.

## Configuration

Requires `.env` with `PLANE_TOKEN` and `PLANE_URL`. Monitors the `Lumina` project by default.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
