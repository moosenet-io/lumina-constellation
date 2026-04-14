# Engram — Semantic Memory System

Engram is Lumina's persistent memory system. It stores and retrieves information across sessions using vector embeddings, allowing Lumina to remember preferences, patterns, and history — and get better the longer it knows you.

**Deploys to:** CT310 at `/opt/lumina-fleet/engram/`
**Inference cost:** $0 — local sqlite-vec embeddings, no cloud calls
**Storage:** SQLite with sqlite-vec extension for vector search

## What Engram Does

Engram provides a key-value + semantic search store, namespaced per agent:

- **Store** — write a fact, preference, or event to memory
- **Search** — find semantically similar memories (vector search)
- **Recall** — retrieve a specific memory by key
- **Forget** — delete a specific memory

## Namespace Isolation

Each agent has its own memory namespace. Lumina cannot read Vigil's private memories unless Vigil explicitly shares them.

| Namespace | Owner | Content |
|-----------|-------|---------|
| `agents/lumina` | Lumina | the operator's preferences, conversation history, patterns |
| `agents/vigil` | Vigil | Briefing history, source quality observations |
| `agents/sentinel` | Sentinel | Alert history, known-good baselines |
| `agents/axon` | Axon | Task patterns, escalation history |
| `shared/household` | All agents | Shared household facts, grocery lists, meal history |

## MCP Tools (in Terminus)

| Tool | Description |
|------|-------------|
| `engram_store(namespace, key, value, tags)` | Store a memory |
| `engram_search(namespace, query, limit)` | Semantic search across a namespace |
| `engram_recall(namespace, key)` | Retrieve a specific memory by key |
| `engram_forget(namespace, key)` | Delete a memory |
| `engram_list(namespace, tags)` | List memories by tag |

## Storage Backend

```python
# sqlite-vec for local vector embeddings
# No cloud embedding API calls
import sqlite_vec

# Embedding model: all-MiniLM-L6-v2 (runs locally via sentence-transformers)
# 384-dimensional embeddings
# Stored in /opt/lumina-fleet/engram/memory.db
```

## How Lumina Uses Engram

At the start of each conversation, Lumina loads context from Engram:

```python
# Automatically loaded into system prompt context
recent_prefs = engram_search('agents/lumina', 'preferences', limit=10)
recent_context = engram_search('agents/lumina', 'recent_events', limit=5)
```

This is how Lumina "remembers" that the operator prefers brief responses, dislikes formal greetings, and is currently tracking a work project — without re-explaining it every session.

## Configuration

```yaml
# constellation.yaml — engram section
engram:
  db_path: /opt/lumina-fleet/engram/memory.db
  embedding_model: all-MiniLM-L6-v2
  max_memories_per_namespace: 10000
  auto_load_namespaces: [agents/lumina, shared/household]
```

## Related

- [Architecture Overview](../architecture/constellation-overview.md)
- sqlite-vec: [github.com/asg017/sqlite-vec](https://github.com/asg017/sqlite-vec)
- MCP tools: `terminus/engram_tools.py`
