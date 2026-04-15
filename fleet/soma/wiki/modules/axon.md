# Axon — Work Queue Manager

Axon is the work queue manager for Lumina Constellation. It monitors the Nexus inbox for incoming work orders, dispatches them to the appropriate agents or tools, and reports results back to Lumina.

**Deploys to:** <fleet-host> at `/opt/lumina-fleet/axon/`
**Trigger:** systemd timer — polls every 60 seconds
**Inference cost:** $0 (pure Python decision logic)

## What Axon Does

1. Polls the Nexus inbox via `nexus_check()` for messages addressed to `axon`
2. Parses the work order (task type, parameters, priority)
3. Routes to the correct handler: Plane work item creation, agent trigger, API call, etc.
4. Marks the message acknowledged with `nexus_ack()`
5. Sends a completion report back to Lumina via `nexus_send()`

Axon never makes LLM calls. Routing decisions use keyword lookup tables and task type fields. If a task is ambiguous, it escalates back to Lumina rather than guessing.

## Files

| File | Purpose |
|------|---------|
| `axon.py` | Main agent script. Inbox polling loop, task router, handler dispatch. |
| `axon.service` | systemd service unit. Managed by <fleet-host>. |

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

## Work Order Format

Axon reads Nexus messages with this structure:

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

## Supported Task Types

| Task Type | Handler | What it does |
|-----------|---------|-------------|
| `create_plane_item` | plane_handler.py | Create a work item in The Plexus |
| `update_plane_item` | plane_handler.py | Update status/priority of existing item |
| `trigger_vigil` | vigil_handler.py | Run a briefing immediately |
| `trigger_sentinel` | sentinel_handler.py | Run an infrastructure health check |
| `run_script` | script_handler.py | Execute a pre-approved script |
| `unknown` | escalate_handler.py | Send back to Lumina for routing |

Unknown task types are escalated back to Lumina rather than silently failing.

## MCP Tools (in Terminus)

| Tool | Description |
|------|-------------|
| `axon_dispatch(task_type, payload, priority)` | Send a work order to Axon via Nexus |
| `axon_status()` | Check Axon's last run time and queue depth |
| `axon_complete(task_id, result)` | Mark a task complete (called by Axon itself) |

## Related

- [Nexus](nexus.md) — the inbox Axon reads
- [The Plexus](../architecture/constellation-overview.md) — Plane CE work queue backend
- [Fleet README](../architecture/constellation-overview.md) — All agents overview
