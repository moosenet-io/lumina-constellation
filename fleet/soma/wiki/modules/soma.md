# Soma — Web Admin Panel

Soma is the web admin panel for Lumina Constellation. It provides module configuration, status monitoring, conversation review, session management, and this wiki. It runs as a FastAPI service on <fleet-host>.

**Deploys to:** <fleet-host> at `/opt/lumina-fleet/soma/`
**Port:** 8082
**Auth:** X-Soma-Key header (set via `SOMA_SECRET_KEY` env var)

## Features

### Status Dashboard (`/`)
- Real-time module health indicators (green / yellow / red)
- Last run time for each agent
- Quick links to logs

### Config (`/config`)
- Agent display name management (naming ceremony)
- Module enable/disable
- API key and endpoint configuration
- Schedule management

### Conversations (`/sessions`)
- Review past IronClaw conversation sessions
- Filter by agent, date, cost
- Inspect individual tool calls

### Skills (`/skills`)
- Browse active and proposed agent skills
- Activate skills from proposed/ to active/
- View SKILL.md content

### Logs (`/logs`)
- Streaming systemd journal for each agent service
- Filter by log level

### Cron (`/cron`)
- View and manage IronClaw routines
- Create/delete scheduled routines
- Manual trigger

### Plugins (`/plugins`)
- Browse MCP tool modules on <terminus-host>
- View tool counts per module
- Enable/disable status

### Wiki (`/wiki`)
- This wiki — built-in documentation
- Searchable, markdown-rendered
- Pages served from `/opt/lumina-fleet/soma/wiki/`

## API Endpoints

All API endpoints require `X-Soma-Key` header.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Service health check (no auth required) |
| `/api/constellation` | GET | Read constellation.yaml |
| `/api/constellation/agent/{id}/display_name` | PUT | Rename an agent |
| `/api/modules` | GET | List module status |
| `/api/sessions` | GET | List conversation sessions |
| `/api/skills` | GET | List agent skills |
| `/api/plugins` | GET | List Terminus plugins |
| `/api/wiki/pages` | GET | List wiki pages |
| `/api/wiki/{path}` | GET | Render a wiki page as HTML |

## Running Soma

```bash
# On <fleet-host>
systemctl status soma
systemctl restart soma
journalctl -u soma -f
```

## Adding Wiki Pages

Drop any `.md` file into `/opt/lumina-fleet/soma/wiki/` (or a subdirectory). Pages are discovered automatically — no server restart needed.

```bash
# Example: add a new guide
echo "# My Guide\n\nContent here." > /opt/lumina-fleet/soma/wiki/guides/my-guide.md
# Refresh wiki page — it appears immediately
```

## Development

Soma is a standard FastAPI application. Templates use Jinja2. Static files (CSS, JS) served from `soma/static/`.

```
soma/
├── api/
│   └── main.py          # FastAPI application
├── templates/           # Jinja2 + plain HTML templates
│   ├── base.html
│   ├── status.html
│   ├── wiki.html
│   └── ...
├── static/              # CSS, JS
│   ├── constellation-reports.css
│   └── ...
├── wiki/                # Wiki markdown pages
│   ├── getting-started/
│   ├── architecture/
│   ├── modules/
│   ├── guides/
│   └── reference/
└── report_template.py   # HTML report builder for modules
```

## Related

- [Getting Started](../getting-started/overview.md)
- [Adding Tools](../guides/adding-tools.md)
- [Creating Skills](../guides/creating-skills.md)
