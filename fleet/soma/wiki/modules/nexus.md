# Nexus — Inter-Agent Inbox

Nexus is the inter-agent inbox for Lumina Constellation. All communication between Lumina and sub-agents routes through Nexus — no direct peer-to-peer messaging between agents.

**Deploys to:** <postgres-host> (Postgres) + <terminus-host> (MCP tools)
**Inference cost:** $0 — pure Python and SQL
**Backend:** PostgreSQL on <postgres-host>

## What Nexus Does

Nexus is a priority-flagged message queue. Lumina writes work orders to Nexus. Sub-agents (primarily Axon) poll Nexus for messages addressed to them. Results flow back to Lumina the same way.

Priority levels:
- `critical` — wakes Lumina immediately
- `urgent` — processed next run cycle
- `normal` — standard queue order
- `low` — background, processed when queue is clear

## MCP Tools (in Terminus)

| Tool | Description |
|------|-------------|
| `nexus_send(to, message, priority)` | Send a message to an agent inbox |
| `nexus_check(agent)` | Check for unread messages addressed to an agent |
| `nexus_read(message_id)` | Read full message content |
| `nexus_ack(message_id)` | Mark a message as acknowledged/processed |
| `nexus_history(agent, limit)` | Read recent message history for an agent |

## Database Schema

```sql
CREATE TABLE inbox (
    id          SERIAL PRIMARY KEY,
    to_agent    TEXT NOT NULL,
    from_agent  TEXT NOT NULL,
    priority    TEXT NOT NULL DEFAULT 'normal',
    subject     TEXT,
    body        JSONB NOT NULL,
    status      TEXT NOT NULL DEFAULT 'unread',
    created_at  TIMESTAMPTZ DEFAULT NOW(),
    read_at     TIMESTAMPTZ,
    acked_at    TIMESTAMPTZ
);

CREATE INDEX idx_inbox_to_agent_status ON inbox(to_agent, status);
CREATE INDEX idx_inbox_created_at ON inbox(created_at DESC);
```

## Message Format

```json
{
  "to": "axon",
  "from": "lumina",
  "priority": "normal",
  "subject": "Create Plane work item",
  "body": {
    "task_type": "create_plane_item",
    "payload": {
      "project": "PX",
      "title": "Research competitor pricing",
      "description": "the operator asked for this at 09:15"
    }
  }
}
```

## Configuration

Nexus reads database credentials from environment variables:

```
INBOX_DB_HOST=<postgres-host>
INBOX_DB_USER=nexus
INBOX_DB_PASS=<from-infisical>
INBOX_DB_NAME=lumina_nexus
```

These are fetched from Infisical (<infisical-host>) by `fetch-mcp-secrets.sh` on <terminus-host> before Terminus starts.

## Architecture Rationale

**No peer-to-peer.** If Vigil wants to notify Sentinel about something, it sends to Lumina via Nexus, and Lumina decides what to do with it. This means any agent can be replaced, restarted, or debugged without breaking the rest of the system.

**Postgres over SQLite.** The inbox needs to be accessible from multiple containers (<ironclaw-host> reads, <fleet-host> writes). SQLite is single-process only.

**Priority flags over complex routing.** The inbox is simple by design. Routing logic lives in Axon, not the queue.

## Related

- [Axon](axon.md) — the agent that reads Nexus and dispatches work
- [Adding Tools](../guides/adding-tools.md) — how nexus_tools.py is structured
- [Architecture Overview](../architecture/constellation-overview.md)
