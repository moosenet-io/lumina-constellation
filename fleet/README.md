# ✦ Fleet

> 14 agents. One fleet. Zero drama.

**Fleet** is Lumina's agent services layer. Each module runs as a separate service or scheduled task, coordinating through Terminus (MCP hub), storing knowledge in Engram, and staying within budget via Myelin.

## Agents

| Module | Motto | Docs |
|--------|-------|------|
| [Axon](axon/) | Work orders in, results out | Execution backbone |
| [Vigil](vigil/) | Good morning | Daily briefings |
| [Sentinel](sentinel/) | 20 checks, 30 minutes | Infrastructure monitoring |
| [Soma](soma/) | Mission control | Dashboard + admin |
| [Vector](vector/) | Ship it | Dev loops + Calx |
| [Myelin](myelin/) | Counts every token | Cost governance |
| [Dura](dura/) | Backup strategy | Secret rotation + resilience |
| [Cortex](cortex/) | Code has opinions | Code intelligence |
| [Obsidian Circle](obsidian_circle/) | Best answer walks out | Multi-model council |
| [Synapse](synapse/) | Right notification | Notification routing |
| [Nexus](nexus/) | Lands here first | Inbox |
| [Seer](seer/) | Reads the whole internet | Web research |
| [Meridian](meridian/) | Paper money, real lessons | Paper trading sandbox |
| [Odyssey](odyssey/) | Path ahead | Travel & logistics |

## Shared infrastructure

- `scheduler.py` — APScheduler jobs (replaces systemd timers for portable deployment)
- `plane_helper.py` — Throttled API client for Plane CE
- `security/` — Secret rotation, PII gate, virtual key generation
- [Shared](shared/) — Common utilities and templates
- [Skills](skills/) — Executable agent capabilities
- [System](system/) — Host integration and homepage

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
