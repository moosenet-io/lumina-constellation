# Axon — Work Queue Manager

Axon is the work queue manager for Lumina Constellation. It monitors the Nexus inbox for incoming work orders, dispatches them to the appropriate agents or tools, and reports results back to Lumina.

**Deploys to:** <fleet-host> (<fleet-server-ip>) at `/opt/lumina-fleet/axon/`
**Trigger:** systemd timer — polls every 60 seconds
**Inference cost:** $0 (pure Python decision logic)

---

## What Axon Does

1. Polls the Nexus inbox via `nexus_check()` for messages addressed to `axon`.
2. Parses the work order (task type, parameters, priority).
3. Routes to the correct handler: Plane work item creation, agent trigger, API call, etc.
4. Marks the message acknowledged with `nexus_ack()`.
5. Sends a completion report back to Lumina via `nexus_send()`.

Axon never makes LLM calls. Routing decisions use keyword lookup tables and task type fields. If a task is ambiguous, it escalates back to Lumina rather than guessing.

---

## Files

| File | Purpose |
|------|---------|
| `axon.py` | Main agent script. Inbox polling loop, task router, handler dispatch. |
| `axon.service` | systemd service unit. Managed by <fleet-host>. |

---

## systemd Service

```ini
[Unit]
Description=Axon Work Queue Manager
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/lumina-fleet/axon
ExecStart=/usr/bin/python3 /opt/lumina-fleet/axon/axon.py
Restart=always
RestartSec=30

[Install]
WantedBy=multi-user.target
```

Manage with standard systemd commands on <fleet-host>:

```bash
systemctl status axon
systemctl restart axon
journalctl -u axon -f
```

---

## Work Order Format

Axon reads Nexus messages with the following structure:

```json
{
  "to": "axon",
  "from": "lumina",
  "priority": "normal",
  "task_type": "create_plane_item",
  "payload": {
    "project": "PX",
    "title": "...",
    "description": "..."
  }
}
```

Supported task types are defined in `axon.py`. Unknown task types are escalated to Lumina.

---

## Architecture

- **Runs on:** <fleet-host> (`<fleet-server-ip>`) at `/opt/lumina-fleet/axon/`
- **Dependencies:** Python 3.11+, `psycopg2` (Nexus inbox), `requests` (Plane API)
- **Connections:** Reads from Nexus inbox (<postgres-host> Postgres); dispatches via Plane API (<plane-host>) and Nexus `nexus_send()`; no direct peer agent connections

## Configuration

Axon reads configuration from environment variables set by the <fleet-host> systemd unit:

| Variable | Purpose | Default |
|----------|---------|---------|
| `INBOX_DB_HOST` | Postgres host for Nexus | <postgres-host> IP |
| `INBOX_DB_USER` | Nexus database user | — |
| `INBOX_DB_PASS` | Nexus database password | — |
| `PLANE_API_URL` | Plane CE base URL | `http://<plane-ip>:8000` |
| `PLANE_API_TOKEN` | Plane API token | — |
| `PLANE_WORKSPACE` | Plane workspace slug | `moosenet` |
| `AXON_POLL_INTERVAL` | Inbox poll interval (seconds) | `60` |

## History / Lineage

Axon was designed in session 11 as the work queue manager for the Nexus inbox system (see `specs/lumina-nexus-prd.docx`, Phase 4). It replaces ad-hoc task delegation that previously required Lumina to make multiple direct Plane API calls per work request. The name comes from the neurological axon — the signal-carrying fiber that transmits decisions to effectors.

Previously known as "Agent Tasker" in early architecture docs.

## Credits

- Nexus inbox system — designed in session 11 (see `specs/lumina-nexus-prd.docx`)
- Plane CE integration — built on `plane_tools.py` patterns from Terminus
- psycopg2 — PostgreSQL adapter: Federico Di Gregorio (LGPL)

## Related

- [fleet/README.md](../README.md) — Fleet overview and agent list
- [terminus/axon_tools.py](../../terminus/axon_tools.py) — MCP tools Lumina uses to dispatch work to Axon
- Nexus inbox — Postgres-backed priority queue on <postgres-host>
