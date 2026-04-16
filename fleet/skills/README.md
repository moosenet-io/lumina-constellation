# ✦ Skills

> Executable capabilities for the agent fleet.

**Skills** contains the executable "skills" that agents can perform, from code review to morning briefings.

## What it does

- Houses standalone executable scripts that represent agent capabilities.
- Organizes skills into `active` (production-ready) and `proposed` (experimental) states.
- Provides a standard execution environment for task-specific logic.
- Enables dynamic skill discovery and invocation by the orchestrator.
- Maintains versioned snapshots of agent logic.

## Key files

| File | Purpose |
|------|---------|
| `active/` | Production-ready skills |
| `proposed/` | Experimental or user-proposed skills |
| `README.md` | Skill development and registration guide |

## Talks to

- **[Soma](../soma/)** — Skills can be proposed and reviewed through Soma.
- **[Axon](../axon/)** — Axon dispatches work orders that invoke these skills.
- **[Terminus](../../terminus/)** — Skills use Terminus tools to interact with the world.

## Configuration

Skills typically require their own environment variables, documented in their respective subdirectories.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
