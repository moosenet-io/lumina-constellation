# Engram — Semantic Memory System

Engram is the persistent memory layer for Lumina Constellation. It stores knowledge, observations, and behavioral patterns as vector embeddings, enabling semantic search across everything the system has learned about you.

**Deploys to:** CT310 (<fleet-server-ip>) at `/opt/lumina-fleet/engram/`
**Storage:** SQLite + sqlite-vec (1536-dimensional embeddings)
**Cost:** $0 — local vector search, no cloud calls for retrieval

---

## What Engram Stores

| Namespace | What it holds | Example |
|-----------|--------------|---------|
| `agents/lumina/profile/*` | Lumina's knowledge of the user | Preferences, communication style, recurring topics |
| `agents/lumina/briefing/*` | Briefing history | Past summaries, what was flagged, follow-ups |
| `agents/vigil/*` | Briefing agent observations | News topics of interest, calendar patterns |
| `agents/axon/*` | Work queue history | Completed tasks, dispatch patterns |
| `agents/sentinel/*` | Infrastructure events | Past alerts, resolution patterns |
| `containers/*` | Container-level observations | Deployment notes, config history |
| `crucible/*` | Learning goals and progress | Courses, books, streaks, milestones |
| `household/*` | Shared household knowledge | Meal preferences, travel wishlist |

Each agent gets its own namespace. Private data (personal health, finances) lives under `agents/{name}/private/*` and is not shared across agents.

---

## Files

```
engram/
├── IDENTITY.md     # Lumina's persistent identity document
├── LUMINA.md       # Behavioral guidelines and delegation instructions
├── USER.md         # What Lumina knows about the user (human-readable)
├── agents/         # Per-agent knowledge files
├── playbooks/      # Repeatable procedures Lumina can reference
└── README.md
```

---

## How Knowledge Is Stored

Agents write to Engram via `engram_tools.py` (Terminus MCP tools):

```python
# Store a fact
engram_store(
    namespace="agents/vigil",
    key="news_preference_sports",
    content="User follows NHL hockey — Maple Leafs and Canucks.",
    tags=["sports", "news", "preference"]
)

# Retrieve semantically similar facts
engram_search(
    namespace="agents/lumina/profile",
    query="what sports does the user follow",
    top_k=5
)
```

Embeddings are generated locally using a small model via Ollama — no cloud API calls for storage or retrieval.

---

## Namespace Structure

```
agents/
  lumina/
    profile/        # Long-term user knowledge
    briefing/       # Briefing session history
    private/        # Sensitive personal data
  vigil/            # Vigil's observations
  axon/             # Axon's task history
  sentinel/         # Infrastructure event log
containers/         # Deployment and config notes
crucible/           # Learning progress
household/          # Shared household context
```

---

## Data in This Repo

The `engram/` directory in this repo contains the **human-readable reference layer** — documents that Lumina reads directly via LUMINA.md and IDENTITY.md. The vector database (SQLite) lives on CT310 and is not committed to git.

Documents here are maintained by hand and during build sessions. They inform Lumina's personality and behavioral defaults before she has built up runtime memory.

---

## History / Lineage

Engram was originally named "Reflexa" and "lumina-memory-repo" — both renamed in session 11 to "Engram" as part of the Lumina naming consolidation. The name evokes engrams — the hypothetical physical trace of a memory in the brain.

The vector database layer (sqlite-vec) was designed in the session 11 Nexus PRD Phase 1 planning. Prior to session 11, Lumina's memory consisted of flat Markdown files in `~/.ironclaw/`. The structured namespace model was introduced to support the multi-agent household (Lumière) without cross-contaminating private data between agents.

## Credits

- [sqlite-vec](https://github.com/asg017/sqlite-vec) — local vector embeddings (MIT/Apache, Alex Garcia)
- A-MEM memory architecture — Huang et al. (2025), arXiv:2502.12110 — Zettelkasten-inspired linked memory with dynamic indexing; influenced Engram's namespace linking model
- Agentic Memory (2026) — Yu et al., arXiv:2601.01885 — unified LTM/STM framework; informed the memory provider interface design

## Related

- [terminus/engram_tools.py](../terminus/engram_tools.py) — MCP tools for reading and writing Engram
- [agents/README.md](../agents/README.md) — Agent definitions (each agent has an engram.namespace)
- [Root README](../README.md) — System overview
