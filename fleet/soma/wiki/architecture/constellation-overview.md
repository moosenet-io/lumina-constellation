# Constellation Architecture Overview

Lumina Constellation is built around three principles: personality-first, inference de-bloating, and no peer-to-peer agent messaging. This document explains how those principles translate into system design.

## Container Layout

The system runs across three Proxmox nodes with dedicated containers:

| Container | Role | Key Services |
|-----------|------|-------------|
| **CT212** (ai-dev-control) | Dev workspace | Claude Code, code-server |
| **CT214** (Terminus) | MCP tool hub | FastMCP server, 200+ tools |
| **CT300** (Postgres) | Database | Nexus inbox backend |
| **CT305** (Lumina) | Lead orchestrator | IronClaw v0.24.0, Refractor proxy |
| **CT306** (messaging) | Matrix server | Tuwunel + bridge bot |
| **CT310** (Fleet) | Agent processes | Vigil, Sentinel, Axon, Vector, Soma |
| **CT315** (Plane) | Work management | Plane CE (The Plexus) |

## How a Request Flows

1. **the operator → Matrix** — Message arrives at CT306 (Tuwunel Matrix server)
2. **Matrix → IronClaw** — The bot bridge delivers the message to Lumina's IronClaw session on CT305
3. **Lumina reasons** — IronClaw invokes a reasoning turn using Refractor (Smart Proxy) on CT305:4000
4. **Refractor filters tools** — The 200+ Terminus tools are filtered to 17–28 relevant tools based on keyword categories
5. **Tool calls → Terminus** — IronClaw connects to Terminus (CT214) via stdio transport (FastMCP)
6. **Results → Nexus** — If delegation is needed, Lumina calls `nexus_send()` to post a work order
7. **Agent picks up** — Axon (on CT310) polls Nexus, reads the work order, executes it
8. **Result routes back** — Agent calls `nexus_send()` back to Lumina, Lumina delivers to the operator via Matrix

## Component Detail

### IronClaw (CT305)

The agent runtime. IronClaw is a security-first Rust implementation with:
- **WASM sandboxing** — every tool runs in its own WebAssembly container
- **Credential isolation** — secrets injected at host boundary, never exposed to tool code
- **Endpoint allowlisting** — HTTP requests only go to explicitly approved hosts

IronClaw connects to Terminus via `stdio.sh`, which sources `.env` then launches `server.py --stdio`.

### Terminus / FastMCP (CT214)

All system capabilities exposed as MCP tools. 20 tool modules, 200+ individual tools. Modules include:

- `nexus_tools.py` — inbox send/read/ack
- `vigil_tools.py` — trigger briefings
- `sentinel_tools.py` — query infrastructure health
- `plane_tools.py` — 24 Plane CE CRUD operations
- `engram_tools.py` — semantic memory store/search

See [Adding Tools](../guides/adding-tools.md) for how to extend Terminus.

### Refractor (CT305:4000)

The Smart Proxy filters Terminus's 200+ tools down to 17–28 per reasoning turn using keyword categories. This keeps each LLM context window lean and reduces per-turn cost.

Categories include: `nexus`, `axon`, `vigil`, `sentinel`, `plane`, `google`, `engram`, and 12+ more.

### Nexus (Postgres on CT300)

The inter-agent inbox. All agent-to-agent communication routes through Nexus — no direct peer-to-peer messaging. Priority flags: `critical`, `urgent`, `normal`, `low`.

Lumina orchestrates, never executes bulk work. Max ~5 tool calls per user request.

### Engram (CT310)

Semantic memory system using sqlite-vec for local vector embeddings. Namespaced per agent so each agent has private memory. Household-level shared memory available via the `shared` namespace.

### Soma (CT310:8082)

FastAPI web admin panel. Provides:
- Module status dashboard
- Agent rename (naming ceremony)
- Conversation review
- Cron/routine management
- Plugin management
- This wiki

## Inference De-Bloating in Practice

Before every function: can Python handle this? If yes, no LLM.

```
         ┌──────────────────┐
         │  Cloud Opus       │  <0.1% — Architecture, security audits
         ├──────────────────┤
         │  Cloud Sonnet /   │  ~2% — Research, synthesis, reasoning
         │  Haiku            │
         ├──────────────────┤
         │  Local models ($0)│  ~8% — Parsing, classification
         ├──────────────────┤
         │  Python +         │  ~90% — API calls, math, SQL,
         │  Templates ($0)   │  templates, cron, threshold checks
         └──────────────────┘
```

See [Inference De-Bloating](inference-de-bloating.md) for the full decision chain.

## Key Design Decisions

**Python first, LLM last.** Templates replace generated text. Lookup tables replace classification. Threshold checks replace AI judgment. 98% of work costs $0.

**Observe and advise, never block.** Myelin tracks costs but never silently stops inference. The human always decides.

**CalDAV/IMAP over OAuth.** Google integration uses a single App Password. Simpler, works in containers, no token refresh complexity.

**Multi-model, multi-provider.** The Obsidian Circle (Mr. Wizard) runs four different AI architectures simultaneously. Their disagreements are genuine.
