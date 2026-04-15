# Module Index

All Lumina Constellation modules. Each module has a dedicated wiki page linked below.

## Core (always-on)

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Nexus** | Inter-agent inbox. Priority queue backed by Postgres. | Python ($0) | [nexus.md](nexus.md) |
| **Axon** | Work queue manager. Polls Nexus, dispatches tasks to agents. | Python ($0) | [axon.md](axon.md) |
| **Engram** | Semantic memory. Namespaced per agent. sqlite-vec local embeddings. | Python ($0) | [engram.md](engram.md) |
| **Pulse** | Event bus and temporal markers. Tracks activity state across agents. | Python ($0) | — |
| **Synapse** | Spontaneous conversation trigger. Surfaces relevant memories and events proactively. | Python + local ($0) | — |

## Agents

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Vigil** | Morning/evening briefings: weather, calendar, commute, news. | Python + local | [vigil.md](vigil.md) |
| **Sentinel** | Cluster health monitoring. Alerts only on failure. | Python ($0) | [sentinel.md](sentinel.md) |
| **Vector** | Autonomous dev loops with feedback gates. | Cloud (Claude Code) | [vector.md](vector.md) |
| **Seer** | Multi-source web research, synthesized reports. | Cloud (tiered) | — |

## Reasoning

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Obsidian Circle** | Deep deliberation council. Four models (Opus, Sonnet, Wizard, Qwen) debate before deciding. | Multi-model | — |

## Infrastructure

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Soma** | Web admin panel. Onboarding wizard, config, conversation review. | FastAPI | [soma.md](soma.md) |
| **Refractor** | Smart LLM proxy. Filters 200+ tools to 17–28 per turn via keyword categories. | Python ($0) | — |
| **Terminus** | MCP tool hub. 20 modules, 200+ tools, FastMCP stdio transport. | Python ($0) | — |
| **The Plexus** | Work queue backed by Plane CE. Structured task dispatch. | Python ($0) | — |
| **Myelin** | Token governance, cost tracking, runaway detection. | Python ($0) | [myelin.md](myelin.md) |
| **Cortex** | Code intelligence: AST analysis, blast radius, review certificates. | Cloud Sonnet | [cortex.md](cortex.md) |
| **Dura** | Backups, smoke tests, log aggregation, secret rotation. | Python ($0) | [dura.md](dura.md) |

## Container Map

| Container | Hosts |
|-----------|-------|
| <ironclaw-host> (Lumina) | Lumina orchestrator, Refractor proxy |
| <terminus-host> (Terminus) | All MCP tools |
| <fleet-host> (Fleet) | Vigil, Sentinel, Axon, Vector, Cortex, Myelin, Dura, Soma, Dashboard |
| <postgres-host> (Postgres) | Nexus inbox, Engram vector store |
| <plane-host> (Plane) | The Plexus work queue |
| <matrix-host> (Messaging) | Matrix server (Tuwunel) |

## Adding a New Module

1. Create `fleet/{name}/` with `{name}.py` and `{name}.service`
2. Create `agents/{name}.agent.yaml` for agent definition
3. Add MCP tools in `terminus/{name}_tools.py`
4. Register in `terminus/server.py`
5. Add a Refractor keyword category
6. Update `LUMINA.md` on <ironclaw-host> with delegation instructions
7. Add a wiki page here
