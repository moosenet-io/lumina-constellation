# Modules

Lumina is organized as a roster of cooperating modules. Each does one thing well, and they
all share the same three-tier memory subsystem and the same MCP tool hub. Modules
are grouped here by the role they play in the system.

## Memory and reasoning

### Memory subsystem
The long-term memory. Stores facts, preferences, decisions, and learned patterns in a local
vector store, retrievable by every module via retrieval-augmented generation. This is the
shared brain: anything one module learns becomes available to the others.

### Reasoning council
A multi-model reasoning council. Convenes several models to deliberate on a hard problem,
each contributing a position, with the strongest model synthesizing a final recommendation.
Used for decisions that benefit from more than one perspective.

### Code intelligence engine
Analyzes repository structure, quality signals, and dependency graphs
using deterministic tooling. Feeds development loops and code-review sessions.

## Execution and routing

### Work-queue executor
Polls a task source, dispatches each item to the module best suited
to handle it, and tracks completion.

### Request router
Smart request routing. Maps an incoming request to the right tool category and inference
tier, so the cheapest capable path is always tried first.

### Cost tracker
Cost governance. Tracks inference spend per consumer with virtual keys, enforces daily
budgets, and trips a circuit breaker when autonomous spend exceeds a threshold.

### Notification router
Takes structured alerts from any module and delivers them through the
configured channel(s) with appropriate urgency.

## Senses

### Research agent
A web-research agent. Decomposes a query, searches, ranks sources (with prompt-injection
defenses), synthesizes a report, and files the findings into the memory subsystem.

### Browser automation
Sandboxed browser automation with a layered security model, human-in-the-loop confirmation,
and session recording for review.

### Scheduled briefing engine
Scheduled briefings. Compiles weather, calendar, news, and system health into a digest and
delivers it on a schedule. Most of the work is deterministic; a local model writes the prose.

### Health monitoring service
Infrastructure monitoring. Runs periodic health checks, exports metrics, and flags anomalies
such as runaway inference.

## System surface

### Inter-agent messaging system
The inbox. Notifications, alerts, and work orders flow through it before being routed to
the right module.

### Tool hub
The MCP tool hub. A single gateway exposing 100+ tools — version control, project tracking,
monitoring, web research, calendar, and more — to every module, with rate limiting and
per-tool timeouts.

### Web dashboard
The dashboard and admin surface: configuration panels, module status, session review, and an
onboarding flow. Authenticated.

### System administration tools
Operational resilience: backup orchestration, smoke testing, secret rotation, and disaster
recovery.

## Optional and lifestyle modules

These modules extend the assistant into specific life areas. Availability varies; some are
fully built, others are specifications.

| Module | Purpose |
|--------|---------|
| **Dev-loop runner** | Autonomous development loops with behavioral correction. |
| **Paper-trading sandbox** | Paper-trading sandbox with a reasoning journal. Never touches real money. |
| **Expense tracker** | Expense tracking via a personal-finance integration. |
| **Vehicle tracker** | Vehicle/maintenance tracking. |
| **Travel planner** | Travel planning. |
| **Health tracker** | Health tracking. |
| **Household manager** | Household management. |

## The orchestrator

### Lumina
The orchestrator itself — a personality-first agent with a persona, a persistent memory, and
a schedule. It owns the chat channel, decides what to do, and delegates execution to the
modules above rather than doing bulk work itself.

---

For how these pieces fit together, see [architecture.md](architecture.md). For the routing
philosophy that keeps most work off the cloud, see
[inference-debloating.md](inference-debloating.md).
