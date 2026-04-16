# ✦ Deployment

> One command. Every module. Your hardware.

**Deployment** contains the orchestration and installation scripts for running Lumina Constellation on diverse hardware.

## What it does

- Automates the installation process for Strix Halo, Apple Silicon, and discrete GPUs.
- Orchestrates multi-container deployments using Docker Compose.
- Manages hardware detection and kernel parameter optimization.
- Provides migration scripts from legacy systems (e.g., OpenClaw).
- Configures the local LiteLLM proxy and Ollama inference stack.

## Key files

| File | Purpose |
|------|---------|
| `install.sh` | Main interactive installer for the constellation |
| `docker-compose.yml` | Container orchestration for all services |
| `detect_hardware.py` | Identifies GPU/NPU capabilities and memory |
| `generate_litellm_config.py` | Produces optimized model routing configs |

## Talks to

- **[Soma](../fleet/soma/)** — Hosts the web dashboard on port 8082.
- **[Sentinel](../fleet/sentinel/)** — Monitors the health of the deployed stack.
- **[Dura](../fleet/dura/)** — Manages backups and secret rotation for the deployment.

## Configuration

Environment variables and secrets managed in `.env`. Hardware-specific presets in `model_presets.yaml`.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
