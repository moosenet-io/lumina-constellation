# Engram — Lumina Semantic Memory System

Engram is the persistent memory and knowledge layer for the Lumina agent constellation. It provides semantic search, activity journaling, and pattern storage using sqlite-vec for local vector embeddings.

## Architecture

```
<fleet-host> /opt/lumina-fleet/engram/
├── engram.py       # Core memory engine (sqlite-vec, 1536-dim embeddings)
├── reflexa.py      # Write queue hooks — T1/T2/T3 triggers for Vector/ARCADE
└── engram.db       # Runtime database (not committed to Gitea)
```

<terminus-host> exposes 5 MCP tools via `engram_tools.py`:
- `engram_query(query, layer, top_k)` — semantic search
- `engram_store(content, layer, key, agent_id)` — store a memory
- `engram_journal(event, agent_id, metadata)` — append to activity journal
- `engram_conventions(agent_id)` — retrieve agent coding conventions
- `engram_recent(agent_id, hours_back)` — recent activity summary

## Three-Layer Model

| Layer | Purpose | Who writes | Namespace |
|-------|---------|------------|-----------|
| `kb` | Factual knowledge (services, configs, patterns) | Lumina | `system/` |
| `journal` | Activity log (what happened, when) | All agents | `agents/{id}/` |
| `patterns` | Behavioral conventions, coding style | Vector | `system/patterns/` |

Personal layers (health, learning, trading, work, commute) are namespaced per agent. Shared layers (grocery, meal-plan, travel, vehicle) are under `household/`.

## Embedding Configuration

- **Backend**: sqlite-vec (local, no external service)
- **Model**: `text-embedding` via LiteLLM proxy (<litellm-host>) — 1536 dimensions
- **Similarity**: Cosine similarity, top-K retrieval
- **Model override**: `REFLEXA_EMBED_MODEL`, `REFLEXA_EMBED_DIM` env vars

## Reflexa Write Hooks (Vector integration)

Engram's `reflexa.py` provides three trigger tiers for Vector's dev loop:

| Tier | Trigger | Content stored |
|------|---------|---------------|
| T1 | Every iteration | Task description, files changed |
| T2 | On decision points | Reasoning trace, alternative paths considered |
| T3 | On completion | Final outcome, cost, lessons learned |

Hooks run via `reflexa_hooks.sh` in Vector's working directory.

## Quick Start

```bash
# On <fleet-host>
cd /opt/lumina-fleet/engram

# Store a memory
python3 engram.py store "Python services use systemd Type=simple" --layer kb --key "systemd-pattern"

# Semantic search
python3 engram.py query "how do we deploy Python services?" --layer kb --top-k 3

# View recent activity
python3 engram.py recent --agent-id lumina --hours-back 24
```

## Deployment Notes

- `engram.db` is created on first run — not committed to Gitea (too large, runtime state)
- sqlite-vec must be installed: `pip3 install sqlite-vec --break-system-packages`
- LiteLLM proxy must be running for embedding generation
- If DB becomes corrupted, delete and let it regenerate on next run

## Future: Qdrant Migration

ENG-62 tracks upgrading from sqlite-vec to Qdrant for multi-agent concurrent writes. Current sqlite-vec implementation is sufficient for single-agent (Lumina) use. Qdrant becomes necessary when 3+ agents write simultaneously.
