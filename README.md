<p align="center">
  <strong>Lumina Constellation</strong><br>
  <em>A personality-first AI personal assistant — 25 modules, under a dollar a day.</em>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-green?style=for-the-badge" alt="License: MIT"></a>
  <a href="https://github.com/moosenet-io"><img src="https://img.shields.io/badge/Built%20by-MooseNet-blueviolet?style=for-the-badge" alt="Built by MooseNet"></a>
  <a href="#modules"><img src="https://img.shields.io/badge/Modules-25-blue?style=for-the-badge" alt="Modules"></a>
  <a href="#cost-breakdown"><img src="https://img.shields.io/badge/Daily%20Cost-Under%20%241-success?style=for-the-badge" alt="Cost"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> •
  <a href="#architecture">Architecture</a> •
  <a href="#modules">Modules</a> •
  <a href="#inference-de-bloating">Inference De-Bloating</a> •
  <a href="#citations--credits">Citations & Credits</a>
</p>

---

## What is Lumina?

Lumina Constellation is a self-hosted, multi-agent AI personal assistant designed for individuals and families. It combines 25 purpose-built modules into a unified system that manages your calendar, briefings, finances, kitchen, health, travel, vehicle maintenance, learning goals, and more — running on commodity hardware with a personality that learns and adapts to you over time.

**Lumina is a personality-first assistant.** Unlike generic chatbots that forget you after every session, Lumina builds a persistent understanding of who you are — your preferences, communication style, schedule, goals, and household. She delegates to specialized sub-agents for execution, but the personality and relationship are continuous.

The core architectural insight is **inference de-bloating**: Python handles ~90% of tasks at zero inference cost, local models handle ~8%, and cloud AI is reserved for the ~2% that requires genuine reasoning. Daily operating cost: under a dollar.

## What makes this different

- **Personality-first** — Lumina is a character, not a service. Name her. Give her a voice. She persists across conversations and gets better the longer she knows you.
- **Inference de-bloating** — 90% of operations use Python or templates at zero cost. LLMs handle judgment only. The system earns every API dollar it spends.
- **Multi-agent household** — Each person gets their own agent. Lumina and Lumière share household context (groceries, meals, travel) while keeping personal data private.
- **Self-hosted backends** — Grocy, Actual Budget, LubeLogger, Honcho. No subscriptions, no external APIs for personal data. Your data stays on your hardware.
- **Agent Skills standard** — Skills are portable ([agentskills.io](https://agentskills.io)). Write once, share across Claude Code, Cursor, Hermes, Goose, and any compatible agent.

## Quick Start

```bash
git clone https://github.com/moosenet-io/lumina-constellation.git
cd lumina-constellation/deploy
docker compose --profile standard up -d
```

Open your browser to `https://localhost` — the Soma onboarding wizard guides you through naming your assistant, connecting your chat platform, configuring AI providers, and selecting modules.

### Deployment Profiles

| Profile | Command | What you get |
|---------|---------|-------------|
| `minimal` | `docker compose --profile minimal up -d` | Core agent + admin panel. For evaluators. |
| `standard` | `docker compose --profile standard up -d` | Everything except local inference. Most users. |
| `gpu` | `docker compose --profile gpu up -d` | Full system including Ollama with GPU passthrough. |
| `headless` | `docker compose --profile headless up -d` | API-only. For integration into existing infrastructure. |

### Requirements

- Docker and Docker Compose
- 4+ CPU cores, 8GB+ RAM (more for local inference)
- Optional: NVIDIA GPU + Container Toolkit for local model serving
- Optional: Cloud API key (Anthropic, OpenRouter) for reasoning tasks

## Architecture

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

### Multi-Agent Household

Lumina supports multiple agents sharing one infrastructure. Each person gets their own assistant with their own personality, calendar, and private data — but household resources like grocery lists, meal plans, travel plans, and budgets are shared.

Each agent is defined by a single `.agent.yaml` file. Adding a new agent: drop one file, start the container, and they're guided through a naming ceremony on first launch.

## Repositories

| Directory | Contents | Deploys to |
|-----------|----------|-----------|
| [terminus/](terminus/) | MCP tool hub — 20 tool modules, FastMCP server | `/opt/ai-mcp/` on your MCP hub |
| [fleet/](fleet/) | All agent processes — Axon, Vigil, Sentinel, Vector, Seer, Cortex, Myelin, Dura, Soma | `/opt/lumina-fleet/` on your fleet host |
| [agents/](agents/) | Agent definitions (`.agent.yaml`) | Reference |
| [engram/](engram/) | Knowledge base, journals, behavioral patterns | `/opt/lumina-fleet/engram/` on your fleet host |
| [deploy/](deploy/) | Docker Compose deployment, Dockerfiles, Caddyfile | Docker host |
| [docs/](docs/) | Built-in help system, module docs, guides | Served by Soma |
| [specs/](specs/) | System design specifications and PRDs | Reference |
| [skills/](skills/) | Agent Skills (agentskills.io format) | Auto-discovered |

## Agent Skills

Lumina uses the [agentskills.io](https://agentskills.io/specification) open standard for skills — portable, shareable task procedures compatible with Claude Code, Cursor, Hermes, Goose, and other agents.

Skills live in `skills/` and are auto-discovered at startup. See [skills/README.md](skills/README.md).

## Modules

### Orchestration

| What it does for you | Module |
|----------------------|--------|
| Priority inbox every 5 min. Critical alerts wake you. Flags: critical / urgent / normal / low. | **Nexus** |
| Remembers you across every session. Preferences, patterns, history. Gets better over time. | **Engram** |
| Picks up tasks, dispatches to agents, reports back. You assign, Axon executes. | **Axon** |
| Your primary interface. Personality-first orchestrator. Delegates so you don't have to. | **Lumina** |
| Multi-model reasoning for hard problems. Four AI architectures deliberate independently. | **Mr. Wizard** |

### Daily Life

| What it does for you | Module |
|----------------------|--------|
| Morning briefing: weather, calendar, commute, news, sports. One summary, zero effort. | **Vigil** |
| Traffic alert when your commute is worse than baseline. Silent when normal. | **Commute** |
| Pantry tracking, recipe matching from what you have, meal planning, shopping lists. | **Hearth** |
| Budget tracking, spending alerts at 50/80/100%. Category reports without subscriptions. | **Ledger** |

### Lifestyle

| What it does for you | Module |
|----------------------|--------|
| Course and book tracking, reading queue, streaks, hobby goals. Learning without guilt. | **Crucible** |
| Bucket list travel, deal monitoring, loyalty point tracking, card portfolio. | **Odyssey** |
| Health data import, coaching nudges, training programs. Templates, not nagging. | **Vitals** |
| Service history, fuel log, maintenance reminders. Your vehicle's memory. | **Relay** |
| Paper trading sandbox with AI reasoning journal. Learn to trade without losing money. | **Meridian** |

### Intelligence

| What it does for you | Module |
|----------------------|--------|
| Multi-source web research, synthesized reports. Seer does the reading. | **Seer** |
| Code intelligence: AST analysis, blast radius, review certificates, audit reports. | **Cortex** |
| Cost tracking per agent. Runaway detection. Observes and advises — never silently blocks. | **Myelin** |
| Daily read-only dashboard: weather, calendar, health grid, cost summary. | **Dashboard** |

### Infrastructure

| What it does for you | Module |
|----------------------|--------|
| Cluster health in 30s. Alerts only on failure. No LLM cost for healthy checks. | **Sentinel** |
| Autonomous dev loops with feedback gates. Vector writes, tests, and commits. | **Vector** |
| Backups, smoke tests, log aggregation, secret rotation, data export. | **Dura** |
| Web admin panel. Onboarding wizard, status dashboard, skills/plugin management, report viewer, chat widget, built-in wiki, Vector management. | **Soma** |
| Smart LLM proxy. 18+ keyword categories reduce tool context per turn. | **Refractor** |
| MCP tool hub. 20 tool modules, FastMCP with stdio transport. | **Terminus** |
| Work queue backed by Plane CE. Structured task dispatch across agents. | **The Plexus** |
| Shared `constellation.css`. Auto light/dark. All HTML surfaces use the same tokens. | **Design System** |

## Inference De-Bloating

Before every function: can Python handle this? If yes, no LLM. Can a local model? If yes, no cloud. Cloud AI only for genuine reasoning.

```
         ┌──────────────────┐
         │  Cloud Opus       │  <0.1% — Architecture, security audits
         ├──────────────────┤
         │  Cloud Sonnet /   │  ~2% — Research, synthesis, reasoning
         │  Haiku            │
         ├──────────────────┤
         │  Local models ($0)│  ~8% — Parsing, classification
         ├──────────────────┤
         │  Python +         │  ~90% — API calls, math, SQL,
         │  Templates ($0)   │  templates, cron, threshold checks
         └──────────────────┘
```

Most "AI tasks" don't need AI. A Python script checking disk usage is faster, cheaper, and more reliable than asking an LLM to do it. Health coaching messages use a pre-written template library, not fresh generation. Budget alerts are threshold comparisons, not inference calls. The system only reaches for AI when it genuinely needs to think.

## Cost Breakdown

| Tier | Frequency | Cost |
|------|-----------|------|
| Python / templates / SQL | ~90% of operations | $0.00 |
| Local Ollama models | ~8% of operations | $0.00 |
| Cloud Haiku / small models | ~2% of operations | ~$0.10–0.30/day |
| Cloud Sonnet | Reasoning tasks | ~$0.20–0.50/day |
| Cloud Opus | Gated — architecture only | Subscription |
| **Total daily target** | | **under $1.00** |

## Key Design Decisions

**Personality-first.** Lumina remembers you. Engram accumulates knowledge across every session. Your assistant gets better over time — not because the model improved, but because it knows you better.

**Python first, LLM last.** Templates replace generated text. Lookup tables replace classification. Threshold checks replace AI judgment. 98% of work costs $0.

**Multi-agent household.** Multiple people, multiple agents, shared infrastructure. Private where personal, shared where helpful. One `.agent.yaml` per agent.

**Observe and advise, never block.** Myelin tracks costs but never silently stops inference. The human always decides.

**CalDAV/IMAP over OAuth.** Google integration uses a single App Password. Simpler, works in containers, no token refresh complexity.

**Multi-model, multi-provider.** The Obsidian Circle runs four different AI architectures simultaneously. Their disagreements are genuine — different training, different reasoning. Swap any provider without changing architecture.

## Repository Structure

```
lumina-constellation/
├── terminus/          # MCP tool hub
│   ├── server.py      # FastMCP server
│   ├── *_tools.py     # 20 tool modules (200+ tools)
│   └── fetch-secrets.sh
├── fleet/             # Agent fleet
│   ├── axon/          # Work queue
│   ├── vigil/         # Briefings
│   ├── sentinel/      # Ops monitoring
│   ├── vector/        # Dev loops
│   ├── seer/          # Research
│   ├── cortex/        # Code intelligence
│   ├── myelin/        # Cost governance
│   ├── dura/          # Resilience
│   └── shared/        # Templates, design system, agent loader
├── agents/            # Agent definitions (.agent.yaml)
├── engram/            # Knowledge base, journals, behavioral patterns
├── deploy/            # Docker deployment
│   ├── docker-compose.yml
│   ├── Dockerfile.*
│   └── Caddyfile
├── docs/              # Built-in help system
│   ├── getting-started/
│   ├── modules/
│   ├── guides/
│   └── reference/
├── skills/            # Agent Skills (agentskills.io format)
├── specs/             # System design specifications
└── README.md
```

## Contributing

We welcome contributions. Browse the [spec library](specs/) for system design context. File issues for bugs or feature requests. Skills can be shared at [agentskills.io](https://agentskills.io).

## Citations & Credits

Lumina Constellation builds on ideas and tools from the broader AI agent ecosystem.

### Architectural Influences

**Ralph Loop Pattern** — Geoffrey Huntley. Autonomous agent loop where a coding agent runs repeatedly against a spec until complete, with memory persisting via git history.
- [ghuntley.com/loop](https://ghuntley.com/loop/) · [snarktank/ralph](https://github.com/snarktank/ralph)

**NPCSH** — NPC Worldwide. Composable multi-agent shell with portable agent definitions, team orchestration, and knowledge graphs. Inspired Lumina's `.agent.yaml` format, conversation review, help system, and Docker deployment.
- [NPC-Worldwide/npcsh](https://github.com/NPC-Worldwide/npcsh) · [npc-shell.readthedocs.io](https://npc-shell.readthedocs.io/)

**SkillClaw** — Ma, Z. et al. (2026). Collective skill evolution in multi-user agent ecosystems. Inspired Lumina's planned skill evolution system.
- [arxiv.org/abs/2604.08377](https://arxiv.org/abs/2604.08377) · [AMAP-ML/SkillClaw](https://github.com/AMAP-ML/SkillClaw)

**Agentic Code Reasoning** — Ugare, S. & Chandra, S., Meta (2026). Semi-formal structured reasoning with certificate templates. Implemented in Lumina's Cortex module and Obsidian Circle.
- [arxiv.org/abs/2603.01896](https://arxiv.org/abs/2603.01896)

**code-review-graph** — Tirth Patel. Tree-sitter AST knowledge graph with blast-radius analysis. Powers Cortex.
- [tirth8205/code-review-graph](https://github.com/tirth8205/code-review-graph)

**Zettelkasten Method** — The interconnected note-taking system that inspired both A-MEM and Engram's knowledge network. Each memory creates structured attributes (context, keywords, tags) and establishes links to related memories, enabling knowledge to evolve as new information is added.

**ARCADE** — Predecessor autonomous dev loop agent. Ralph-inspired loop with behavioral correction (Calx), feedback gates, and inference tier selection. Vector is ARCADE's successor, ported to the Lumina infrastructure with pluggable backends, Nexus integration, and agentskills.io skill discovery.
- [Archived](https://github.com/LeMajesticMoose/arcade)

### Runtime & Frameworks

**IronClaw** — [NEAR AI](https://github.com/nearai/ironclaw). Lumina's agent runtime. IronClaw is a Rust-based, security-first reimplementation of OpenClaw focused on privacy and data sovereignty. We chose IronClaw over OpenClaw for three reasons: (1) WASM sandbox isolation — every tool runs in its own WebAssembly container with capability-based permissions, (2) credential protection — secrets are injected at the host boundary and never exposed to tool code, and (3) endpoint allowlisting — HTTP requests only go to explicitly approved hosts. For a system managing personal calendars, finances, and health data, these aren't optional. IronClaw runs with 272+ tools connected via MCP.

| Project | Role in Lumina | Source |
|---------|---------------|--------|
| **IronClaw** | Agent runtime — WASM sandboxing, credential isolation, endpoint allowlisting | [nearai/ironclaw](https://github.com/nearai/ironclaw) |
| **FastMCP** | MCP server framework for Terminus | [jlowin/fastmcp](https://github.com/jlowin/fastmcp) |
| **LiteLLM** | Unified LLM proxy (100+ providers) | [BerriAI/litellm](https://github.com/BerriAI/litellm) |
| **Ollama** | Local model serving | [ollama/ollama](https://github.com/ollama/ollama) |
| **Caddy** | Automatic HTTPS reverse proxy | [caddyserver/caddy](https://github.com/caddyserver/caddy) |

### Self-Hosted Backends

| Project | Powers module | Source |
|---------|--------------|--------|
| **Actual Budget** | Ledger (finance) | [actualbudget/actual](https://github.com/actualbudget/actual) |
| **Grocy** | Hearth (kitchen) | [grocy/grocy](https://github.com/grocy/grocy) |
| **LubeLogger** | Relay (vehicle) | [hargata/lubelog](https://github.com/hargata/lubelog) |
| **SearXNG** | Seer (research) | [searxng/searxng](https://github.com/searxng/searxng) |
| **Homepage** | Dashboard | [gethomepage/homepage](https://github.com/gethomepage/homepage) |
| **Plane CE** | The Plexus (work queue) | [makeplane/plane](https://github.com/makeplane/plane) |
| **Tuwunel** | Matrix communication | [matrix-construct/tuwunel](https://github.com/matrix-construct/tuwunel) |

### Academic References

- Ugare, S. & Chandra, S. (2026). "Agentic Code Reasoning." arXiv:2603.01896.
- Ma, Z. et al. (2026). "SkillClaw: Let Skills Evolve Collectively with Agentic Evolver." arXiv:2604.08377.
- Huang, J. et al. (2023). "Large Language Models Cannot Self-Correct Reasoning Yet." arXiv:2310.01798.
- Ugare, S. & Chandra, S. (2026). "Agentic Code Reasoning." arXiv:2603.01896.
- Xu, W., Liang, Z., Mei, K., Gao, H., Tan, J., & Zhang, Y. (2025). "A-MEM: Agentic Memory for LLM Agents." NeurIPS 2025. arXiv:2502.12110. Zettelkasten-inspired memory with dynamic indexing, linking, and memory evolution. Influenced Engram's interconnected knowledge network design.
- Yu, Y., Yao, L., Xie, Y., Tan, Q., Feng, J., Li, Y., & Wu, L. (2026). "Agentic Memory: Learning Unified Long-Term and Short-Term Memory Management for LLM Agents." arXiv:2601.01885. Unified LTM/STM framework with memory operations as tool-based actions and progressive RL training. Informed Lumina's memory provider interface design.
- Hardwick, S. (2026). "The Behavioral Plane: Why Learned Corrections Don't Transfer Between Agents." Zenodo. DOI: 10.5281/zenodo.19142179. Explains why behavioral corrections (like Calx triggers) are agent-local and don't generalize across different LLM architectures. Informed Vector's per-project guardrails scoping design.

### Further Reading

- "Long-term Memory in Agentic Systems" — Moxo (moxo.com/blog/agentic-ai-memory). Two-layer memory architecture patterns for production agents.
- "7 Steps to Mastering Memory in Agentic AI Systems" — Machine Learning Mastery (machinelearningmastery.com). Practical implementation guide.
- "What Is AI Agent Memory?" — IBM Think (ibm.com/think/topics/ai-agent-memory). Enterprise perspective on agent memory components.

## Contributors

- **Peter Boose** ([@LeMajesticMoose](https://github.com/LeMajesticMoose)) — Creator, architect, and product lead. Designed and directed the entire system via voice transcription and AI-assisted development.
- **Claude** ([Anthropic](https://anthropic.com)) — Co-developer. Specifications, implementation, autonomous build sessions, and infrastructure debugging via Claude Code.

## Disclaimer

Lumina Constellation is self-hosted software that integrates with services managing sensitive personal data — including health records (Vitals), financial accounts (Ledger/Actual Budget), vehicle information (Relay/LubeLogger), and household inventory (Hearth/Grocy). **You are solely responsible for securing your deployment.**

By using this software, you acknowledge:

- **No warranty.** This software is provided "as is" under the MIT License, without warranty of any kind. The authors are not liable for any data loss, security breach, or damages arising from its use.
- **Security is your responsibility.** Self-hosted deployments require proper network isolation, firewall configuration, credential management, and regular updates. The default configuration prioritizes ease of setup, not hardened security.
- **Sensitive data at rest.** Engram, Honcho, and other memory systems store personal information in local databases. Encrypt your disks, restrict physical access, and maintain backups.
- **AI outputs are not professional advice.** Health information from Vitals is not medical advice. Financial data from Ledger is not financial advice. Trading signals from Meridian are not investment advice. Always consult qualified professionals for medical, financial, and legal decisions.
- **Third-party API keys.** Cloud inference providers, calendar services, news APIs, and other integrations require API keys that grant access to external accounts. Store these in a secrets manager (e.g., Infisical), never in code or config files.
- **LLM limitations apply.** AI agents can hallucinate, misinterpret instructions, and take unintended actions. Human review of agent-initiated changes — especially to financial, health, or infrastructure systems — is strongly recommended.

Use at your own risk.

## License

MIT License. See [LICENSE](LICENSE) for details.

Copyright (c) 2026 Peter Boose

---

<p align="center">
  <em>Personality-first AI · Powered by inference de-bloating · Under a dollar a day</em>
</p>
