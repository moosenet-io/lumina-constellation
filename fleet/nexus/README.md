# ✦ Nexus

> "Everything lands here first."

**Nexus** is Lumina's inter-agent inbox — every notification, alert, work order, and message flows through Nexus before being routed to the right handler. It's the single communication backbone that lets agents talk to each other without peer-to-peer messaging.

## What it does

- **Inbox for all agents**: Axon, Vigil, Sentinel, Myelin, Synapse all send and receive via Nexus
- **Work order dispatch**: Lumina sends work orders to Axon via Nexus; Axon reports completion back
- **PostgreSQL backend**: persistent message store with indexing, acknowledgment, and history
- **Priority queue**: urgent, high, normal, low — processed in order
- **No peer-to-peer**: agents never contact each other directly; all routing through Nexus

## Key files

| File | Purpose |
|------|---------|
| `inbox_monitor.py` | Polls inbox for pending messages, dispatches to handlers |
| `nexus_sqlite.py` | Local SQLite cache for offline/low-latency access |
| `household_config.py` | Household routing configuration (legacy) |
| `household_routing.py` | Message routing rules (legacy) |

## Talks to

- **Terminus** (`nexus_tools.py`) — MCP tools: `nexus_send`, `nexus_check`, `nexus_read`, `nexus_ack`, `nexus_history`
- **Axon** — receives work orders, sends completion receipts
- **Sentinel** — sends alert messages
- **Myelin** — sends budget alert messages
- **Synapse** — receives notification routing requests

## Configuration

```bash
INBOX_DB_HOST=your-postgres-host
INBOX_DB_USER=lumina_inbox_user
INBOX_DB_PASS=...
```

Database: `lumina_inbox` on PostgreSQL. Schema applied via `fleet/nexus/schema.sql`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
