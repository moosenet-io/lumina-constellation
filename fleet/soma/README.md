# Soma — Lumina Admin Panel

> Web-based administration interface and first-run onboarding wizard for Lumina Constellation.

**Deploys to:** <fleet-host> (`YOUR_FLEET_SERVER_IP`) at `/opt/lumina-fleet/soma/`  
**Service:** `soma.service` (systemd)  
**Port:** 8082 (HTTP direct) or via Caddy reverse proxy

---

## Quick Start

```bash
# Soma is already running if you followed the deployment guide.
# Check status:
systemctl status soma.service

# Access:
http://YOUR_FLEET_SERVER_IP:8082
```

Default admin token: `soma-dev-key` (change via `SOMA_SECRET_KEY` env var)

---

## Pages

| URL | Description |
|-----|-------------|
| `/` | Dashboard → status |
| `/status` | System health dashboard — agents, services, inference cost |
| `/setup` | Onboarding wizard — 8-step first-run setup |
| `/config` | Configuration editor — YAML config, Refractor keywords |
| `/skills` | Skills management — active, proposed, disabled |
| `/plugins` | Plugin management — MCP plugins |
| `/sessions` | Conversation history viewer |
| `/cron` | Systemd timer management |
| `/logs` | Real-time log viewer |
| `/wiki` | Built-in documentation wiki |
| `/vector` | Vector autonomous dev loop management |
| `/reports` | HTML report viewer for all modules |
| `/council` | Obsidian Circle deliberation interface |
| `/synapse` | Synapse outreach history and config |

---

## API Reference

All API endpoints require `X-Soma-Key` header (set `SOMA_SECRET_KEY` env var).

### Status & Health

```
GET /health              — Basic health check (no auth)
GET /api/status          — Comprehensive system status
GET /api/system/health   — Legacy health endpoint
GET /api/modules         — List all fleet modules
GET /api/inference/status — LLM proxy status
GET /api/cost            — Today's inference cost
```

### Configuration

```
GET  /api/constellation              — Read constellation.yaml
PUT  /api/constellation/agent/{id}/display_name — Rename an agent
GET  /api/config/{module}            — Read module config
```

### Skills & Plugins

```
GET    /api/skills              — List all skills
POST   /api/skills/{name}/approve  — Approve a proposed skill
DELETE /api/skills/{name}          — Delete a skill
GET    /api/plugins             — List <terminus-host> plugins
```

### Logs & Sessions

```
GET /api/logs?service={name}&lines={n}  — Get log lines
GET /api/sessions                        — List recent conversations
GET /api/timers                          — List systemd timers
POST /api/timers/{name}/trigger          — Manually run a timer
```

### Vector

```
GET  /api/vector/status    — Current Vector task status
GET  /api/vector/calx      — Calx trigger history
GET  /api/vector/prs       — Open PRs created by Vector
POST /api/vector/submit    — Submit a work order to Vector
```

### Chat

```
POST /api/chat             — Proxy message to Lumina via IronClaw
GET  /api/chat/test        — Test IronClaw connectivity
```

### Wiki & Reports

```
GET /api/wiki/pages        — List wiki pages
GET /api/wiki/{path}       — Render a wiki page as HTML
GET /api/reports           — List HTML reports by module
```

### Caching

All API endpoints serve from a 10s–60s TTL in-memory cache (see `cache.py`). Use `POST /api/cache/clear` to force refresh.

---

## Authentication

Soma uses a single admin token for all endpoints (no user accounts in v1).

```bash
# Set in environment:
SOMA_SECRET_KEY=your-secret-token

# Use in requests:
curl -H "X-Soma-Key: your-secret-token" http://YOUR_FLEET_SERVER_IP:8082/api/status
```

The default token `soma-dev-key` is for development only. Change before production use.

---

## How it deploys

Soma is a FastAPI application running as a systemd service on <fleet-host>.

```
/opt/lumina-fleet/soma/
├── api/main.py          # FastAPI application (all endpoints)
├── templates/           # HTML page templates (served as FileResponse)
│   ├── status.html      # Status dashboard
│   ├── wizard.html      # Onboarding wizard (8-step)
│   ├── skills.html      # Skills management
│   ├── sessions.html    # Session history
│   ├── cron.html        # Timer management
│   ├── logs.html        # Log viewer
│   ├── wiki.html        # Documentation wiki
│   ├── vector.html      # Vector management
│   └── partials/
│       └── chat.html    # Chat widget (included in all pages)
├── static/
│   └── constellation-reports.css  # Report styling extension
├── wiki/                # Markdown documentation pages
├── report_template.py   # Report generation helper
└── soma_review.py       # Conversation review engine
```

Service definition: `/etc/systemd/system/soma.service`

---

## Development

To add a new page:

1. Create `templates/yourpage.html` using constellation.css classes
2. Add a route to `api/main.py`: `@app.get("/yourpage")` → `return _file_page("yourpage.html")`
3. Add to the nav in `templates/base.html` (if using the shared nav layout)
4. Restart: `systemctl restart soma.service`
5. Add a soma_tools.py entry if Lumina should be able to access the page via MCP

---

## History / Lineage

Soma was designed in session 11 as the first-run onboarding interface for public deployment of Lumina Constellation. Before Soma, configuration required manual `.env` editing and SSH access. The admin panel was conceived alongside the Docker deployment profile to make the system accessible to non-technical users. The naming ceremony (step 1 of the wizard) came from the multi-agent household model — every Lumina instance should have a unique name and personality.

The name "Soma" refers to the neurological soma (cell body) — the central hub that integrates signals from all dendrites (agents) and directs the output.

## Credits

- FastAPI — [tiangolo/fastapi](https://github.com/tiangolo/fastapi) (MIT, Sebastián Ramírez)
- Design system — `constellation.css` (all templates use it — no inline styles)
- Wiki system — Markdown rendering via Python `markdown` library
- Onboarding wizard pattern — inspired by [NPCSH](https://github.com/NPC-Worldwide/npcsh) first-run UX

## Related

- [Root README](../../README.md) — Project overview
- [fleet/README.md](../README.md) — Agent fleet overview
- [skills/README.md](../../skills/README.md) — Agent Skills standard
