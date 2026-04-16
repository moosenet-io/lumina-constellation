# ✦ Scripts

> Operational tools and automation.

**Scripts** contains utility scripts for repository maintenance, privacy auditing, and system operations.

## What it does

- Performs repository-wide PII and secrets scans to ensure privacy.
- Automates repetitive maintenance tasks and data migrations.
- Provides helper scripts for agent-led development sessions.
- Generates system-wide reports and metrics for the operator.
- Validates repository health and convention compliance.

## Key files

| File | Purpose |
|------|---------|
| `privacy_scan.py` | Scans for PII, secrets, and sensitive data |
| `README.md` | This documentation |

## Talks to

- **[Terminus](../terminus/)** — Scripts often invoke MCP tools for data extraction.
- **[Soma](../fleet/soma/)** — Feeds audit results into the dashboard.
- **[Dura](../fleet/dura/)** — Assists in secret rotation and backup verification.

## Configuration

Settings for individual scripts are typically passed via CLI arguments or environment variables.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
