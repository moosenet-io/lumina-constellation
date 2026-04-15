# Deploy — Docker Deployment

This directory contains everything needed to run Lumina Constellation via Docker Compose. Four deployment profiles cover use cases from evaluation to full GPU-accelerated production.

---

## Quick Start

```bash
git clone https://github.com/moosenet-io/lumina-constellation.git
cd lumina-constellation/deploy
cp .env.example .env        # Add your API keys and secrets
docker compose --profile standard up -d
```

Open `https://localhost` — Soma's onboarding wizard takes it from there.

---

## Deployment Profiles

| Profile | Command | RAM needed | What you get |
|---------|---------|-----------|-------------|
| `minimal` | `docker compose --profile minimal up -d` | 4GB | Core agent + Soma admin panel. Best for evaluating Lumina. |
| `standard` | `docker compose --profile standard up -d` | 8GB | Full system minus local inference. Most users start here. |
| `gpu` | `docker compose --profile gpu up -d` | 16GB + GPU | Everything including Ollama with GPU passthrough. |
| `headless` | `docker compose --profile headless up -d` | 4GB | API only, no web UI. For integration into existing infrastructure. |

---

## Requirements

| Requirement | Minimum | Recommended |
|-------------|---------|-------------|
| Docker | 24.x | Latest |
| Docker Compose | 2.x | Latest |
| CPU | 4 cores | 8+ cores |
| RAM | 4GB | 16GB |
| Disk | 20GB | 100GB |
| GPU (optional) | NVIDIA, 6GB VRAM | 12GB+ VRAM |
| CUDA (optional) | 11.x | 12.x + NVIDIA Container Toolkit |

---

## First-Run Experience

On first boot, Soma guides you through a setup wizard:

1. **Naming ceremony** — Choose a name and personality for your assistant.
2. **Channel connection** — Connect Matrix (or other chat platform).
3. **AI provider config** — Add Anthropic / OpenRouter API keys. Local-only mode available.
4. **Module selection** — Enable the modules you want. Each can be toggled individually.
5. **Backend connections** — Connect Grocy, Actual Budget, LubeLogger if you have them running.

After setup, you can talk to your assistant via Matrix or the Soma web interface.

---

## Environment Variables

Copy `.env.example` to `.env` and fill in your values. Required variables depend on which modules you enable. Secrets are never committed — `.env` is in `.gitignore`.

Key variables:

| Variable | Used by | Required for |
|----------|---------|-------------|
| `ANTHROPIC_API_KEY` | Lumina, Cortex, Seer | Cloud reasoning |
| `OPENROUTER_API_KEY` | Obsidian Circle | Multi-model council |
| `MATRIX_TOKEN` | All Matrix-connected agents | Chat delivery |
| `PLANE_API_TOKEN` | Axon, Plexus | Work queue |
| `GOOGLE_APP_PASSWORD` | Vigil, Cortex | Calendar + email |

---

## Architecture Notes

- **Caddy** handles TLS termination automatically (Let's Encrypt or self-signed).
- **Postgres** backs the Nexus inbox. Data persists in a named Docker volume.
- **Ollama** runs as a separate container. GPU profile passes through the NVIDIA device.
- Agents on <fleet-host> connect to Terminus (<terminus-host>) via MCP stdio — not needed in the Docker deployment, which bundles both.

---

## Stopping and Updating

```bash
docker compose --profile standard down    # Stop
docker compose --profile standard pull    # Pull latest images
docker compose --profile standard up -d   # Restart
```

Data volumes persist across restarts. To reset completely: `docker compose down -v` (destroys all data).

---

## Migrating from OpenClaw

If you're coming from an existing OpenClaw installation, the migration tool moves your personality, user profile, memory, and skills to Lumina's equivalent locations.

```bash
# Dry-run first — shows what will be migrated without making changes
python3 deploy/migrate-from-openclaw.py

# Execute migration
python3 deploy/migrate-from-openclaw.py --execute

# Custom OpenClaw path
python3 deploy/migrate-from-openclaw.py --source /path/to/openclaw --execute
```

**What migrates:**

| OpenClaw | Lumina | Notes |
|----------|--------|-------|
| `SOUL.md` | `~/.ironclaw/LUMINA.md` | Personality / system prompt |
| `USER.md` | `~/.ironclaw/USER.md` | User profile |
| `MEMORY.md` + `memory/` | Engram | Parsed as facts, requires manual import |
| `skills/` | `/opt/lumina-fleet/skills/active/` | agentskills.io format, copied directly |
| `.env` API keys | Infisical | Printed to console only — add manually |

OpenClaw data is never modified. The migration is read-only from the source.

---

## IronClaw Vault Setup

IronClaw uses an encrypted local vault (libSQL) for LLM credentials. The vault takes priority over `.env`. On a fresh install, the vault is empty — `ironclaw-setup.sh` seeds it from your environment variables so `docker compose up` works without a manual `ironclaw onboard` step.

```bash
# Run after docker compose up (or include in your entrypoint)
source .env && bash deploy/ironclaw-setup.sh
```

**Required environment variables:**

| Variable | Description |
|----------|-------------|
| `LLM_API_KEY` | API key for your LLM provider (or LiteLLM master key) |
| `LLM_BASE_URL` | OpenAI-compatible endpoint URL |
| `LLM_MODEL` | Model name (e.g. `claude-sonnet`, `gpt-4o-mini`) |

**Vault credential resolution (in order):**

1. `ironclaw-setup.sh` seeds the vault from env vars on each start
2. If vault is populated, IronClaw uses it (existing behavior for interactive installs)
3. If neither env nor vault has credentials, IronClaw prompts via `onboard --step provider`

**If IronClaw still prompts for credentials after running the setup script:**
```bash
ironclaw onboard --step provider   # Run once interactively to force-seed the vault
```

---

## Related

- [Root README](../README.md) — Architecture overview and module list
- [fleet/README.md](../fleet/README.md) — Agent fleet details
- [docs/getting-started/installation.md](../docs/getting-started/installation.md) — Detailed installation guide
