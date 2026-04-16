# ✦ Nexus

> Everything lands here first.

**Nexus** is the inbox and routing hub for incoming communications and household requests.

## What it does

- Monitors the constellation's central "Inbox" for new requests and data.
- Routes household-specific tasks to the appropriate sub-modules.
- Integrates with Plane (Plexus) for project and task management.
- Provides a unified interface for managing household configurations.
- Archives incoming data streams for processing by other agents.

## Key files

| File | Purpose |
|------|---------|
| `inbox_monitor.py` | Polls for new messages and requests |
| `household_routing.py` | Rules for dispatching tasks to specialized agents |
| `nexus_sqlite.py` | Local storage for the inbox and transient data |
| `household_config.py` | Management of shared household settings |

## Talks to

- **[Axon](../axon/)** — Forwards validated requests for execution.
- **[Soma](../soma/)** — Displays the current inbox and household status.
- **[Synapse](../synapse/)** — Sends notifications about new urgent inbox items.

## Configuration

Inbox polling interval and routing rules defined in `household_config.py`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
