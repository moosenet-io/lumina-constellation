# ✦ Cortex

> "Your code has opinions about itself. Cortex reads them."

**Cortex** is Lumina's code intelligence module — it analyzes repository structure, quality metrics, and dependency graphs so Vector and the Obsidian Circle can reason about code without reading every file.

## What it does

- **Repository structure analysis**: module map, file counts, dependency graph
- **Code quality metrics**: complexity, test coverage, lint status, blast radius estimation
- **Audit reports**: generates HTML reports with constellation.css styling
- **Integrates with Vector**: provides code context for autonomous dev loops
- **Integrates with Circle**: council members can query code structure before deliberating

## Key files

| File | Purpose |
|------|---------|
| `cortex.py` | Main analysis engine — repo scanning, metrics, HTML report generation |

## Talks to

- **Vector** — provides repository context for development loops
- **Obsidian Circle** — answers code structure queries during deliberation
- **Terminus** (`cortex_tools.py`) — MCP tools expose Cortex via IronClaw

## Configuration

```bash
FLEET_DIR=/opt/lumina-fleet
GITEA_URL=http://your-gitea-host:3000
GITEA_TOKEN=...
```

Reports output to `cortex/output/` as HTML files with the constellation design system.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
