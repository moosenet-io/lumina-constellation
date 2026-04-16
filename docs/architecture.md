# Architecture

> How 25 modules talk to each other without stepping on each other's toes.

## The stack

Lumina runs as a set of cooperating services. On a single box, everything is Docker Compose except Ollama (which runs natively for direct GPU access).

```
Peter (Matrix / Element on phone)
  │
  ▼
Lumina orchestrator (IronClaw, ironclaw-host)
  ├── Terminus MCP hub (272+ tools)
  │     ├── spectra_tools (browser)
  │     ├── engram_tools (memory)
  │     ├── plane_tools (project mgmt)
  │     ├── gitea_tools (version control)
  │     ├── prometheus_tools (monitoring)
  │     └── ... 33 more tool modules
  ├── LiteLLM proxy (model routing)
  │     ├── Local Ollama (Qwen3.5 fleet) ← $0
  │     ├── OpenRouter (cloud fallback) ← $/token
  │     └── Anthropic API (frontier) ← $/token
  └── Fleet services
        ├── Axon (work queue)
        ├── Vigil (briefings)
        ├── Sentinel (monitoring)
        ├── Engram (memory, sqlite-vec)
        ├── Spectra (browser, Playwright)
        ├── Vector (dev loops)
        ├── Soma (dashboard, port 8082)
        └── ... 12 more services
```

## Deployment modes

| Mode | Who it's for | What it looks like |
|------|-------------|-------------------|
| **All-in-one** | Most people | One box, Docker Compose + native Ollama |
| **Split inference** | Power users | GPU box + services box, same LAN |
| **Distributed** | Homelab enthusiasts | Multi-node Proxmox/k8s cluster |

All three modes use the same Docker Compose file. The only difference is whether `OLLAMA_HOST` points to localhost or a remote IP.

## Key design decisions

**Ollama stays outside Docker.** Unified memory architectures (Strix Halo, Apple Silicon) need native GPU access. Docker GPU passthrough on ROCm is unreliable.

**One MCP hub.** Terminus hosts all 272+ tools. Every agent connects to the same hub. No tool duplication.

**Engram is the shared brain.** Every module that produces knowledge writes to Engram. Every module that needs knowledge queries Engram. sqlite-vec for embeddings, Zettelkasten for linking.

**Python for 90%.** Most tasks don't need an LLM. Python scripts handle weather fetching, Plane API calls, timer scheduling, health checks. LLMs only activate when reasoning is genuinely required.

See the [full module list](modules.md) for what each module does.
