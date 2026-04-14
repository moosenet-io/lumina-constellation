# Self-Contained Deployment Plan

> Lumina Constellation can ship as a fully self-contained Docker Compose bundle. This page documents the planned architecture.

## Vision

A user should be able to run Lumina with a single command:

```bash
git clone https://github.com/moosenet-io/lumina-constellation
cd lumina-constellation/deploy
cp .env.example .env
docker compose --profile standard up -d
```

Open `https://localhost` and the Soma wizard guides them through the rest.

## Bundled Components

| Component | Role | Image |
|-----------|------|-------|
| **IronClaw** | Agent runtime (WASM sandboxed) | `ghcr.io/nearai/ironclaw:latest` |
| **Terminus** | MCP tool hub (250+ tools) | Built from `fleet/terminus/` |
| **Fleet agents** | Axon, Vigil, Sentinel, Vector | Built from `fleet/` |
| **Soma** | Admin panel + onboarding wizard | Built from `fleet/soma/` |
| **LiteLLM** | Unified AI proxy | `ghcr.io/berriai/litellm:main-latest` |
| **Ollama** | Local model serving (optional) | `ollama/ollama:latest` |
| **Postgres** | Nexus inbox + data | `postgres:17-alpine` |
| **Redis** | Caching + Honcho | `redis:7-alpine` |
| **Caddy** | HTTPS termination | `caddy:2-alpine` |

## Deployment Profiles

```bash
# Minimal: Core agent + Soma. For evaluation.
docker compose --profile minimal up -d

# Standard: Full system, cloud LLM. Most users start here.
docker compose --profile standard up -d

# GPU: Everything + Ollama with GPU passthrough. Self-hosted inference.
docker compose --profile gpu up -d

# Headless: API only, no web UI. For integration into existing infrastructure.
docker compose --profile headless up -d
```

## First-Run Experience

When Ollama is bundled, the wizard auto-detects local models and pre-populates LLM config — **no API key needed for basic operation**.

```
docker compose up → open https://localhost → Soma wizard →
  Step 1: Welcome
  Step 2: Name your assistant (naming ceremony)
  Step 3: Auto-scan (all services green — bundled!)
  Step 4: Chat platform (Matrix bundled via Tuwunel)
  Step 5: LLM config (Ollama pre-populated if GPU profile)
  Step 6: Google Calendar (optional)
  Step 7: Modules (enable what you want)
  Step 8: Launch → first message in chat
```

## Inference Without API Keys

The `gpu` profile bundles Ollama. With a capable GPU (8GB+ VRAM), the full stack runs at **$0/day** after initial setup.

Recommended models for GPU deployment:
- **Agent model**: `qwen2.5:7b` (4-bit quantized, 4.5GB VRAM)
- **Code model**: `qwen2.5-coder:7b-instruct` (coding tasks)
- **Embeddings**: `nomic-embed-text` (Engram vector search)

The `standard` profile uses cloud inference (Anthropic Claude or OpenRouter) for reasoning tasks while keeping routine operations local.

## Resource Requirements

| Profile | RAM | VRAM | Disk | Notes |
|---------|-----|------|------|-------|
| minimal | 4GB | 0 | 10GB | Cloud LLM required |
| standard | 8GB | 0 | 20GB | Cloud LLM required |
| gpu | 16GB | 8GB+ | 40GB | Fully self-hosted |
| headless | 2GB | 0 | 5GB | API only |

## Implementation Status

This is a **design document**. The Docker Compose files are under active development in `deploy/`. Current state:

- ✅ `deploy/docker-compose.yml` — skeleton exists
- ✅ Individual service Dockerfiles — in progress
- ⏳ Bundled LiteLLM + Ollama config — planned
- ⏳ Wizard auto-detection of bundled services — planned
- ⏳ One-command install — planned for Session 15+

## Contributing

If you're building Lumina on a new platform (ARM, NixOS, etc.) and hit issues with the Docker deployment, please [open an issue](https://github.com/moosenet-io/lumina-constellation/issues).
