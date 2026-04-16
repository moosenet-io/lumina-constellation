# ✦ Engram (Root)

> She remembers everything. You just have to ask.

**Engram** is the data and storage layer for Lumina Constellation's memory and knowledge.

## What it does

- Stores agent-specific memory partitions and conversation history.
- Houses deployment playbooks for the Reflexa memory engine.
- Manages the persistence layer for the constellation's shared brain.
- Provides a structured directory for Zettelkasten-style knowledge linking.

## Key files

| File | Purpose |
|------|---------|
| `agents/` | Agent-specific memory and state storage |
| `playbooks/` | Ansible playbooks for memory service deployment |
| `README.md` | This documentation |

## Talks to

- **[Fleet Engram](../fleet/engram/)** — Provides the logic for interacting with this data.
- **[Lumina](../agents/)** — Stores the primary agent's long-term memory.
- **[Infra](../infra/)** — Integrated with the constellation's Ansible workflows.

## Configuration

Storage paths typically configured via `ENGRAM_DATA_DIR` in the environment.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
