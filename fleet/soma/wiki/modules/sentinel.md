# Sentinel — Infrastructure Health Monitor

Sentinel monitors the configured service deployment and all running services. It runs on a 5-minute timer and sends alerts only when something is wrong — no LLM cost for healthy checks.

**Deploys to:** <fleet-host> at `/opt/lumina-fleet/sentinel/`
**Trigger:** systemd timer — every 5 minutes
**Inference cost:** $0 (pure Python — no LLM calls)

## What Sentinel Does

1. Runs health checks against all configured targets (containers, services, disks, APIs)
2. Compares results against thresholds defined in config
3. If all checks pass: writes a status JSON file and exits. No alert, no Matrix message.
4. If any check fails: generates an alert, writes HTML status report, sends Matrix notification
5. Tracks alert history to suppress duplicate alerts (configurable cooldown)

**Design principle: silent when healthy, loud when broken.** Sentinel should never become noise.

## Files

| File | Purpose |
|------|---------|
| `ops.py` | Main orchestrator. Runs check suite, evaluates results, dispatches alerts. |
| `health_checks.py` | Individual check functions: disk, memory, service ping, HTTP endpoint, container status. |
| `status_generator.py` | HTML status page generator. Uses `constellation.css`. |

## Health Check Types

| Check | What it monitors | Alert threshold |
|-------|-----------------|-----------------|
| Disk usage | <dev-host>, <terminus-host>, <ironclaw-host>, <fleet-host>, <plane-host> | >85% |
| Memory | Per-container memory usage | >90% |
| Service ping | Matrix, Gitea, Plane, LiteLLM | HTTP 200 within 5s |
| Container status | All configured runtime targets via Prometheus | Running state |
| Prometheus | Scrape freshness | Last scrape >10 min |
| IronClaw | API health endpoint on <ironclaw-host> | HTTP 200 |

## Alert Delivery

Alerts are sent as Matrix messages via Lumina (through Nexus inbox with `priority: critical`). The HTML status page is written regardless of health state — it powers the dashboard's health grid.

## Adding a New Check

Add a function to `health_checks.py`:

```python
def check_myservice(config: dict) -> CheckResult:
    """Returns CheckResult(ok=True/False, name="myservice", detail="...")"""
    try:
        r = requests.get(config['myservice_url'], timeout=5)
        return CheckResult(ok=r.status_code == 200, name='myservice', detail=f"HTTP {r.status_code}")
    except Exception as e:
        return CheckResult(ok=False, name='myservice', detail=str(e))
```

Then register it in `ops.py`'s check list.

## MCP Tools (in Terminus)

| Tool | Description |
|------|-------------|
| `sentinel_health()` | Run a full health check, return summary |
| `sentinel_alert(service, message)` | Manually trigger an alert for a service |
| `sentinel_status()` | Read the last status JSON without running checks |

## systemd Timer

```ini
[Unit]
Description=Sentinel health check run

[Service]
Type=oneshot
ExecStart=/usr/bin/python3 /opt/lumina-fleet/sentinel/ops.py

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min

[Install]
WantedBy=timers.target
```

## Related

- [Fleet Overview](../architecture/constellation-overview.md)
- [Inference De-Bloating](../architecture/inference-de-bloating.md) — why Sentinel uses zero LLM
- MCP tools: `terminus/sentinel_tools.py`
