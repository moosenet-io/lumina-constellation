# ✦ Agents

> She has a name. She has opinions. She remembers yours.

**Agents** is the identity and personality layer of Lumina Constellation, defining who the agents are and how they interact.

## What it does

- Defines agent personalities (Lumina, Lumière) through specialized YAML configurations.
- Manages agent personas for different interaction contexts (Coding, Research, Household).
- Stores core values, communication styles, and behavioral constraints for the fleet.
- Orchestrates the transition between different agent identities based on the task.
- Ensures consistent "voice" and tone across all constellation communications.

## Key files

| File | Purpose |
|------|---------|
| `lumina.agent.yaml` | Core orchestrator personality and constraints |
| `lumiere.agent.yaml` | Partner agent identity and specialized role |
| `wandering-fool.agent.yaml` | Experimental/creative persona |
| `architect-arcane.agent.yaml` | Technical architecture specialist |

## Talks to

- **[Terminus](../terminus/)** — Agents use Terminus tools to interact with the world.
- **[Engram](../engram/)** — Personalities are informed by historical memory.
- **[Obsidian Circle](../fleet/obsidian_circle/)** — Individual agents serve as council members.

## Configuration

Personality traits and system prompts defined in individual `.agent.yaml` files.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
