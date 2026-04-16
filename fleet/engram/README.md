# ✦ Engram

> She remembers everything. You just have to ask.

**Engram** is the long-term memory and knowledge store that serves as the shared brain for all Lumina agents.

## What it does

- Stores facts, decisions, and patterns in a vector-enabled SQLite database (`sqlite-vec`).
- Implements a Zettelkasten-inspired linking system for connecting related ideas.
- Provides Retrieval-Augmented Generation (RAG) context to agents during tasks.
- Archives conversation history and web research findings for future recall.
- Automatically extracts and stores "memories" from agent interactions.

## Key files

| File | Purpose |
|------|---------|
| `engram.py` | Main API for memory storage and retrieval |
| `PATTERNS.md` | Documentation of the Zettelkasten linking patterns |
| `agents/` | Agent-specific memory partitions |

## Talks to

- **[Terminus](../../terminus/)** — Provides memory search and storage tools to agents.
- **[Obsidian Circle](../obsidian_circle/)** — Supplies relevant context for council deliberations.
- **[Soma](../soma/)** — Powers the "Activity Feed" and "Fact Index" on the dashboard.

## Configuration

Database location and embedding model settings configured in `engram.py` or via environment variables.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
