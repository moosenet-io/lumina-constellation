# Agents — Agent Definitions

This directory contains `.agent.yaml` definition files for every agent in Lumina Constellation. Each file is the single source of truth for an agent's identity, capabilities, and deployment.

---

## Current Agents

| File | Agent | Role | Container |
|------|-------|------|-----------|
| `lumina.agent.yaml` | **Lumina** | Primary orchestrator. Personality-first personal assistant. | CT305 |

Additional agents are defined in `/opt/lumina-fleet/agents/` on CT310 and will be added here as the monorepo consolidation progresses.

---

## .agent.yaml Format

```yaml
name: vigil                          # Internal codename — stable, never changes
display_name: "Vigil"                # User-visible name — set during naming ceremony
role: "Morning and evening briefings"
description: |
  Vigil aggregates weather, calendar, commute, and news data into a
  structured daily briefing. Delivers via Matrix and HTML dashboard.

model: "local/qwen"                  # Preferred inference route
model_fallback: "openrouter/haiku"   # Fallback if local unavailable

container: CT310                     # Where this agent runs
deploy_path: /opt/lumina-fleet/vigil/
service: vigil.service               # systemd service name

tools:                               # MCP tool categories this agent uses
  - google
  - news
  - commute
  - hearth
  - engram

engram:
  namespace: agents/vigil            # Memory isolation — never shares with other agents
  shared_namespace: household        # Can read (not write) household context

schedule: "0 7,17 * * *"            # When triggered (cron format)
inference_budget_daily: 0.05        # Max daily spend in USD for this agent
```

---

## Field Reference

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Stable internal codename. Used in code, logs, namespaces. Never change after creation. |
| `display_name` | Yes | User-visible name. Set during naming ceremony, can be updated via Soma. |
| `role` | Yes | One-line description of what this agent does. |
| `model` | Yes | Preferred model route. Format: `provider/model`. |
| `model_fallback` | No | Fallback if preferred model is unavailable. |
| `container` | Yes | CT number where this agent runs. |
| `deploy_path` | Yes | Absolute path on the container. |
| `service` | No | systemd service name, if the agent runs as a daemon. |
| `tools` | Yes | List of MCP tool categories this agent needs. Informs Refractor. |
| `engram.namespace` | Yes | Memory isolation prefix. All engram reads/writes scoped here. |
| `engram.shared_namespace` | No | Optional shared namespace (read-only). |
| `schedule` | No | Cron expression, if agent is timer-triggered. |
| `inference_budget_daily` | No | Soft daily spend cap in USD. Myelin observes and alerts. |

---

## Multi-Agent Household Model

Lumina Constellation supports multiple agents sharing one infrastructure. Example household:

```yaml
# lumina.agent.yaml — the operator's agent
name: lumina
display_name: "Lumina"
engram:
  namespace: agents/lumina
  shared_namespace: household

# lumiere.agent.yaml — Partner's agent
name: lumiere
display_name: "Lumière"
engram:
  namespace: agents/lumiere
  shared_namespace: household
```

Each person's private data (health, finances, personal calendar) lives in their own namespace. Shared data (grocery lists, meal plans, travel, household budget) lives in `household/` and both agents can read it.

---

## Adding a New Agent

1. Create `{name}.agent.yaml` in this directory following the format above.
2. Verify discovery: `python3 /opt/lumina-fleet/shared/agent_loader.py`
3. Create the agent directory in `fleet/{name}/`.
4. Write the agent script and systemd service.
5. Deploy to the target container and enable the service.
6. `naming.py` picks up the new agent automatically — no code changes needed.

---

## Reading Agent Definitions in Code

Always use `agent_loader.py` — never read YAML files directly:

```python
from shared.agent_loader import display_name, get_agent, load_agents

# Get display name (respects naming ceremony overrides)
name = display_name('vigil')        # → "Vigil" or custom name

# Get full agent config
agent = get_agent('vigil')          # → dict with all fields

# List all agents
agents = load_agents()              # → list of agent dicts
```

---

## History / Lineage

The `.agent.yaml` format was introduced in session 11 to replace `constellation.yaml` (a single monolithic config file) as the source of truth for agent identity. The multi-agent household model — where each person gets a named agent with private namespace isolation — drove the design: you can't express per-agent memory namespaces cleanly in one shared config file.

The format was influenced by [NPCSH](https://github.com/NPC-Worldwide/npcsh)'s NPC definition format, adapted to fit IronClaw's tool-category model and Terminus's Refractor keyword routing.

## Credits

- Agent definition format — influenced by [NPC-Worldwide/npcsh](https://github.com/NPC-Worldwide/npcsh) NPC definition conventions
- Agent loader — `shared/agent_loader.py` (Lumina internal)
- Naming ceremony — `fleet/naming_ceremony.py`

## Related

- [fleet/README.md](../fleet/README.md) — Agent fleet overview
- [fleet/shared/agent_loader.py](../fleet/shared/agent_loader.py) — Agent loader module
- [fleet/naming_ceremony.py](../fleet/naming_ceremony.py) — Naming ceremony script
- [Root README](../README.md) — System architecture
