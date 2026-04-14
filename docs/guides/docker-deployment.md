# Docker Deployment Guide — Lumina Constellation

Deploy the full Lumina personal AI operating system on any Docker host in 3 commands.

Source repo: https://github.com/moosenet-io/lumina-deploy

## Quick start

```bash
git clone https://github.com/moosenet-io/lumina-deploy.git
cd lumina-deploy
cp .env.example .env && nano .env   # add ANTHROPIC_API_KEY
docker compose --profile minimal up -d
```

Open https://localhost — the Soma onboarding wizard walks you through the rest.

## Deployment profiles

Four profiles let you scale from a lightweight API-only install to a full GPU-backed stack.

| Profile | Services included | When to use |
|---------|------------------|-------------|
| `minimal` | postgres, soma, terminus, gateway, caddy | First deploy, testing, low-resource hosts |
| `standard` | + actual-budget, grocy | Daily use — adds personal finance and kitchen management |
| `gpu` | + ollama (NVIDIA) | Local Qwen inference, no cloud API needed |
| `headless` | terminus + gateway only, no Soma UI | API-only / programmatic access, embedded mode |

### Switching profiles

```bash
# Start with minimal
docker compose --profile minimal up -d

# Upgrade to standard (adds finance + kitchen)
docker compose --profile standard up -d

# GPU (requires NVIDIA Container Toolkit)
docker compose --profile gpu up -d

# Headless API-only
docker compose --profile headless up -d
```

## Service map

| Service | Internal port | Exposed via | Description |
|---------|--------------|-------------|-------------|
| Soma | 8082 | Caddy :443/soma | Admin panel + onboarding wizard |
| Gateway | 8080 | Caddy :443/api | API aggregator, auth layer |
| Terminus | 4000 | Direct :4000 | MCP tool hub (FastMCP) |
| Postgres | 5432 | Internal only | Nexus inbox + metadata |
| Caddy | 443/80 | Host ports | HTTPS reverse proxy, auto TLS |
| Actual Budget | 5006 | Caddy :443/actual | Personal finance (standard+) |
| Grocy | 9283 | Caddy :443/hearth | Kitchen management (standard+) |
| Ollama | 11434 | Direct :11434 | Local inference (gpu profile) |

## Environment variables

Copy `.env.example` to `.env`. Minimum required:

```bash
# Required
ANTHROPIC_API_KEY=sk-ant-...

# Security — change all of these before first start
POSTGRES_PASSWORD=change-me
SOMA_SECRET_KEY=change-me
DASHBOARD_API_KEY=change-me
LITELLM_MASTER_KEY=sk-change-me
```

Optional variables (Google Calendar, OpenRouter, Actual Budget) are documented in `.env.example`.

## Postgres schema

The Nexus inbox schema (`init/postgres-init.sql`) is applied automatically on first startup via Docker's `docker-entrypoint-initdb.d` mechanism. No manual migration needed.

## Health checks

```bash
# All containers healthy?
docker compose ps

# Gateway
curl -k https://localhost/api/health -H "X-API-Key: your-key"

# Soma
curl -k https://localhost/soma/health
```

## Updating

```bash
git pull
docker compose --profile minimal up -d --build
```

## Related

- Architecture: `docs/architecture/` in this repo
- Nexus inbox spec: `specs/nexus-prd.md`
- Terminus MCP tools: moosenet/lumina-terminus
- Agent fleet: moosenet/lumina-fleet
