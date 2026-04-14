# What is Lumina Constellation?

Lumina Constellation is a self-hosted, multi-agent AI personal assistant designed for individuals and families. It combines 25 purpose-built modules into a unified system that manages your calendar, briefings, finances, kitchen, health, travel, vehicle maintenance, learning goals, and more — running on commodity hardware with a personality that learns and adapts to you over time.

## The Core Idea

**Lumina is a personality-first assistant.** Unlike generic chatbots that forget you after every session, Lumina builds a persistent understanding of who you are — your preferences, communication style, schedule, goals, and household. She delegates to specialized sub-agents for execution, but the personality and relationship are continuous.

The core architectural insight is **inference de-bloating**: Python handles ~90% of tasks at zero inference cost, local models handle ~8%, and cloud AI is reserved for the ~2% that requires genuine reasoning. Daily operating cost: under a dollar.

## What Makes This Different

- **Personality-first** — Lumina is a character, not a service. Name her. Give her a voice. She persists across conversations and gets better the longer she knows you.
- **Inference de-bloating** — 90% of operations use Python or templates at zero cost. LLMs handle judgment only. The system earns every API dollar it spends.
- **Multi-agent household** — Each person gets their own agent. Agents share household context (groceries, meals, travel) while keeping personal data private.
- **Self-hosted backends** — Grocy, Actual Budget, LubeLogger. No subscriptions, no external APIs for personal data. Your data stays on your hardware.
- **Agent Skills standard** — Skills are portable ([agentskills.io](https://agentskills.io)). Write once, share across Claude Code, Cursor, Hermes, Goose, and any compatible agent.

## System Architecture

```
┌─────────────────────────────────────────────────────┐
│              Lumina (Personal Assistant)             │
│       Personality-first · Remembers · Delegates     │
├──────────┬──────────┬──────────┬────────────────────┤
│   Axon   │  Vigil   │ Sentinel │   Vector / Seer    │
│  (work)  │(briefing)│  (ops)   │  (dev) / (research)│
└────┬─────┴────┬─────┴────┬─────┴────────┬───────────┘
     │          │          │              │
┌────▼──────────▼──────────▼──────────────▼───────────┐
│              Terminus (MCP Tool Hub)                 │
│        20 tool files · 200+ tools · FastMCP          │
├─────────────────────────────────────────────────────┤
│  Nexus (Inbox)  │  Engram (Memory)  │  Refractor    │
│  Flag-graded    │  Multi-namespace  │  Smart proxy   │
│  priority queue │  knowledge store  │  18 categories │
└─────────────────┴───────────────────┴───────────────┘
```

Lumina runs on [IronClaw](https://github.com/nearai/ironclaw), a security-first agent runtime with WASM sandboxing, credential isolation, and endpoint allowlisting.

## The 25 Modules

### Orchestration
| Module | What it does |
|--------|-------------|
| **Lumina** | Primary interface. Personality-first orchestrator. Delegates so you don't have to. |
| **Nexus** | Priority inbox every 5 min. Critical alerts wake you. Flags: critical / urgent / normal / low. |
| **Engram** | Remembers you across every session. Preferences, patterns, history. Gets better over time. |
| **Axon** | Picks up tasks, dispatches to agents, reports back. You assign, Axon executes. |
| **Mr. Wizard** | Multi-model reasoning for hard problems. Four AI architectures deliberate independently. |

### Daily Life
| Module | What it does |
|--------|-------------|
| **Vigil** | Morning briefing: weather, calendar, commute, news. One summary, zero effort. |
| **Commute** | Traffic alert when your commute is worse than baseline. Silent when normal. |
| **Hearth** | Pantry tracking, recipe matching from what you have, meal planning, shopping lists. |
| **Ledger** | Budget tracking, spending alerts at 50/80/100%. Category reports without subscriptions. |

### Infrastructure
| Module | What it does |
|--------|-------------|
| **Sentinel** | Cluster health in 30s. Alerts only on failure. No LLM cost for healthy checks. |
| **Vector** | Autonomous dev loops with feedback gates. Vector writes, tests, and commits. |
| **Dura** | Backups, smoke tests, log aggregation, secret rotation, data export. |
| **Soma** | Web admin panel. Onboarding wizard, module config, conversation review, built-in help. |
| **Refractor** | Smart LLM proxy. 18+ keyword categories reduce tool context per turn. |
| **Terminus** | MCP tool hub. 20 tool modules, FastMCP with stdio transport. |
| **The Plexus** | Work queue backed by Plane CE. Structured task dispatch across agents. |

## Next Steps

- [First Five Minutes](first-five-minutes.md) — After `docker compose up`
- [Architecture Overview](../architecture/constellation-overview.md) — Deep dive into system design
- [Module Index](../modules/index.md) — All 25 modules with links to full docs
