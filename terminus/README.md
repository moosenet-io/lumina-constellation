# ✦ Terminus

> 272 tools. One hub. Zero drama.

**Terminus** is the central Model Context Protocol (MCP) hub that hosts all tools used by the Lumina Constellation.

## What it does

- Hosts over 272+ tools across 30+ specialized modules (Gitea, GitHub, Plane, etc.).
- Provides a unified interface for agents to interact with the external world.
- Implements a PII Gate to prevent sensitive data from leaking to cloud models.
- Manages tool discovery, loading, and execution permissions.
- Logs all tool invocations for auditing and debugging.

## Key files

| File | Purpose |
|------|---------|
| `server.py` | The main MCP server and hub orchestration |
| `pii_gate.py` | Sanitizes tool inputs and outputs for privacy |
| `*_tools.py` | Individual tool modules (e.g., `plane_tools.py`, `gitea_tools.py`) |
| `plugin_loader.py` | Dynamically loads external tool plugins |

## Talks to

- **[Lumina](../../agents/)** — Provides the primary interface for agent capabilities.
- **[Engram](../../engram/)** — Offers tools for memory storage and search.
- **[Plexus (Plane)](../fleet/nexus/)** — Provides tools for managing tasks and projects.

## Configuration

Tool modules enabled via `server.py`. PII rules defined in `pii_gate.py`.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
