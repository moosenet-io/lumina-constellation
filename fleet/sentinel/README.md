# Sentinel — Infrastructure Health Monitor

Sentinel monitors the MooseNet Proxmox cluster and all running services. It runs on a 5-minute timer and sends alerts only when something is wrong — no LLM cost for healthy checks.

**Deploys to:** CT310 (<fleet-server-ip>) at `/opt/lumina-fleet/sentinel/`
**Trigger:** systemd timer — every 5 minutes
**Inference cost:** $0 (pure Python — no LLM calls)

---

## What Sentinel Does

1. Runs health checks against all configured targets (containers, services, disks, APIs).
2. Compares results against thresholds defined in config.
3. If all checks pass: writes a status JSON file and exits. No alert, no Matrix message.
4. If any check fails: generates an alert, writes HTML status report, sends Matrix notification.
5. Tracks alert history to suppress duplicate alerts (configurable cooldown).

The design principle: **silent when healthy, loud when broken**. Sentinel should never become noise.

---

## Files

| File | Purpose |
|------|---------|
| `ops.py` | Main agent script. Orchestrates check runs, evaluates results, dispatches alerts. |
| `health_checks.py` | Individual check functions: disk, memory, service ping, HTTP endpoint, container status. |
| `status_generator.py` | HTML status page generator. Uses `constellation.css`. Written to a known path for the dashboard. |

---

## Health Check Types

| Check | What it monitors | Alert threshold |
|-------|-----------------|-----------------|
| Disk usage | CT212, CT214, CT305, CT310, CT315 | >85% |
| Memory | Per-container memory usage | >90% |
| Service ping | Key services: Matrix, Gitea, Plane, LiteLLM | HTTP 200 within 5s |
| Container status | All Proxmox containers via Prometheus | Running state |
| Prometheus | Prometheus scrape freshness | Last scrape >10 min |
| IronClaw | API health endpoint on CT305 | HTTP 200 |

---

## Alert Delivery

Alerts are sent as Matrix messages via Lumina (through Nexus inbox with `priority: critical`). The HTML status page is written regardless of health state — it powers the dashboard's health grid.

---

## Adding a New Check

Add a function to `health_checks.py` following the existing pattern:

```python
def check_myservice(config: dict) -> CheckResult:
    # Returns CheckResult(ok=True/False, name="myservice", detail="...")
    ...
```

Then register it in `ops.py`'s check list. No LLM changes needed.

---

## Architecture

- **Runs on:** CT310 (`<fleet-server-ip>`) at `/opt/lumina-fleet/sentinel/`
- **Dependencies:** Python 3.11+, `requests`, `psutil`; Prometheus scrape endpoint for container metrics
- **Connections:** HTTP checks against CT212, CT214, CT223, CT305, CT310, CT315; Prometheus (CT222); alert delivery via Nexus inbox to Lumina

## Configuration

| Variable | Purpose | Default |
|----------|---------|---------|
| `PROMETHEUS_URL` | Prometheus base URL | `http://<prometheus-ip>:9090` |
| `SENTINEL_ALERT_COOLDOWN` | Seconds before re-alerting same check | `1800` |
| `SENTINEL_DISK_THRESHOLD` | Disk usage alert threshold (%) | `85` |
| `SENTINEL_MEM_THRESHOLD` | Memory alert threshold (%) | `90` |
| `SENTINEL_HTTP_TIMEOUT` | HTTP check timeout (seconds) | `5` |
| `SENTINEL_HTML_PATH` | Where to write the status page HTML | `/var/www/html/status.html` |
| `INBOX_DB_HOST` | Nexus Postgres host (for alert delivery) | CT300 IP |
| `INBOX_DB_USER` | Nexus database user | — |
| `INBOX_DB_PASS` | Nexus database password | — |

## History / Lineage

Sentinel descends from "Agent Ops" (`moosenet/agent-ops`), renamed in session 11 as part of the Lumina naming consolidation. The original `ops.py` was built in session 4 as a simple ping checker; `health_checks.py` was extracted in session 6 when the check count grew beyond a single file. The HTML status page was added in session 8 alongside the `constellation.css` design system rollout. Alert deduplication (cooldown) was added in session 9 after Sentinel spammed Matrix with repeated disk alerts.

## Credits

- Design system — `constellation.css` (see `fleet/shared/`)
- Prometheus metrics integration — Prometheus Python client patterns

## Related

- [fleet/README.md](../README.md) — Fleet overview
- [terminus/sentinel_tools.py](../../terminus/sentinel_tools.py) — MCP tools for querying Sentinel status from Lumina
- [fleet/shared/constellation.css](../shared/constellation.css) — Shared design system used by status_generator.py
