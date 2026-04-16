# ✦ Plugins

> Extending the constellation's reach.

**Plugins** contains optional extensions and integrations for Lumina Constellation.

## What it does

- Houses third-party integrations and experimental features.
- Provides a standard interface for extending agent capabilities.
- Allows for modular addition of new tools and sensors.
- Facilitates community-driven enhancements to the constellation.
- Manages plugin lifecycle and permission isolation.

## Key files

| File | Purpose |
|------|---------|
| `README.md` | Plugin development guide and index |

## Talks to

- **[Terminus](../terminus/)** — Plugins are typically loaded as MCP tool extensions.
- **[Soma](../fleet/soma/)** — Displays active plugins and their status.
- **[Lumina](../agents/)** — Agents interact with the world through these extensions.

## Configuration

Plugins are enabled/disabled via the main configuration or the Soma dashboard.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
