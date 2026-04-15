# Module Index

All 25 Lumina Constellation modules. Each module has a dedicated wiki page linked below.

## Orchestration

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Lumina** | Lead orchestrator. Personality-first. Delegates to sub-agents. | Cloud Sonnet | — |
| **Nexus** | Inter-agent inbox. Priority queue backed by Postgres. | Python ($0) | [nexus.md](nexus.md) |
| **Engram** | Semantic memory. Namespaced per agent. sqlite-vec local embeddings. | Python ($0) | [engram.md](engram.md) |
| **Axon** | Work queue manager. Polls Nexus, dispatches tasks to agents. | Python ($0) | [axon.md](axon.md) |
| **Mr. Wizard** | Deep reasoning. The Obsidian Circle: four models deliberate. | Multi-model | — |

## Daily Life

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Vigil** | Morning/evening briefings: weather, calendar, commute, news. | Python + local | [vigil.md](vigil.md) |
| **Commute** | Traffic alert when commute is worse than baseline. | Python ($0) | — |
| **Hearth** | Pantry tracking, recipe matching, meal planning, shopping lists. | Python + local | — |
| **Ledger** | Budget tracking, spending alerts, category reports via Actual Budget. | Python ($0) | — |

## Lifestyle

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Crucible** | Course/book tracking, reading queue, streaks, hobby goals. | Python ($0) | — |
| **Odyssey** | Bucket list travel, deal monitoring, loyalty point tracking. | Python + local | — |
| **Vitals** | Health data import, coaching nudges, training programs. | Python + template | — |
| **Relay** | Vehicle service history, fuel log, maintenance reminders. | Python ($0) | — |
| **Meridian** | Paper trading sandbox with AI reasoning journal. | Cloud Sonnet | — |

## Intelligence

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Seer** | Multi-source web research, synthesized reports. | Cloud (tiered) | — |
| **Cortex** | Code intelligence: AST analysis, blast radius, review certificates. | Cloud Sonnet | [cortex.md](cortex.md) |
| **Myelin** | Token governance, cost tracking, runaway detection. | Python ($0) | [myelin.md](myelin.md) |
| **Dashboard** | Read-only daily view: weather, calendar, health grid, cost summary. | Python ($0) | — |

## Infrastructure

| Module | Role | Inference | Docs |
|--------|------|-----------|------|
| **Sentinel** | Cluster health monitoring. Alerts only on failure. | Python ($0) | [sentinel.md](sentinel.md) |
| **Vector** | Autonomous dev loops with feedback gates. | Cloud (Claude Code) | [vector.md](vector.md) |
| **Dura** | Backups, smoke tests, log aggregation, secret rotation. | Python ($0) | [dura.md](dura.md) |
| **Soma** | Web admin panel. Onboarding wizard, config, conversation review. | FastAPI | [soma.md](soma.md) |
| **Refractor** | Smart LLM proxy. Filters 200+ tools to 17–28 per turn. | Python ($0) | — |
| **Terminus** | MCP tool hub. 20 modules, 200+ tools, FastMCP stdio transport. | Python ($0) | — |
| **The Plexus** | Work queue backed by Plane CE. Structured task dispatch. | Python ($0) | — |

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
