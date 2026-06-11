# Modules

Lumina is organized as a roster of cooperating modules. Each does one thing well, and they
all share the same three-tier memory (Engram) and the same MCP tool hub (Terminus). Modules
are grouped here by the role they play in the system.

## Memory and reasoning

### Engram
The long-term memory. Stores facts, preferences, decisions, and learned patterns in a local
vector store, retrievable by every module via retrieval-augmented generation. This is the
shared brain: anything one module learns becomes available to the others.

### Obsidian Circle
A multi-model reasoning council. Convenes several models to deliberate on a hard problem,
each contributing a position, with the strongest model synthesizing a final recommendation.
Used for decisions that benefit from more than one perspective.

### Cortex
Code intelligence. Analyzes repository structure, quality signals, and dependency graphs
using deterministic tooling. Feeds development loops and code-review sessions.

## Execution and routing

### Axon
The work-queue executor. Polls a task source, dispatches each item to the module best suited
to handle it, and tracks completion.

### Refractor
Smart request routing. Maps an incoming request to the right tool category and inference
tier, so the cheapest capable path is always tried first.

### Myelin
Cost governance. Tracks inference spend per consumer with virtual keys, enforces daily
budgets, and trips a circuit breaker when autonomous spend exceeds a threshold.

### Synapse
Notification routing. Takes structured alerts from any module and delivers them through the
configured channel(s) with appropriate urgency.

## Senses

### Seer
A web-research agent. Decomposes a query, searches, ranks sources (with prompt-injection
defenses), synthesizes a report, and files the findings into Engram.

### Spectra
Sandboxed browser automation with a layered security model, human-in-the-loop confirmation,
and session recording for review.

### Vigil
Scheduled briefings. Compiles weather, calendar, news, and system health into a digest and
delivers it on a schedule. Most of the work is deterministic; a local model writes the prose.

### Sentinel
Infrastructure monitoring. Runs periodic health checks, exports metrics, and flags anomalies
such as runaway inference.

## System surface

### Nexus
The inbox. Notifications, alerts, and work orders flow through Nexus before being routed to
the right module.

### Terminus
The MCP tool hub. A single gateway exposing 100+ tools — version control, project tracking,
monitoring, web research, calendar, and more — to every module, with rate limiting and
per-tool timeouts.

### Soma
The dashboard and admin surface: configuration panels, module status, session review, and an
onboarding flow. Authenticated.

### Dura
Operational resilience: backup orchestration, smoke testing, secret rotation, and disaster
recovery.

## Optional and lifestyle modules

These modules extend the assistant into specific life areas. Availability varies; some are
fully built, others are specifications.

| Module | Purpose |
|--------|---------|
| **Vector** | Autonomous development loops with behavioral correction. |
| **Meridian** | Paper-trading sandbox with a reasoning journal. Never touches real money. |
| **Ledger** | Expense tracking via a personal-finance integration. |
| **Relay** | Vehicle/maintenance tracking. |
| **Odyssey** | Travel planning. |
| **Vitals** | Health tracking. |
| **Hearth** | Household management. |

## The orchestrator

### Lumina
The orchestrator itself — a personality-first agent with a persona, a persistent memory, and
a schedule. It owns the chat channel, decides what to do, and delegates execution to the
modules above rather than doing bulk work itself.

---

For how these pieces fit together, see [architecture.md](architecture.md). For the routing
philosophy that keeps most work off the cloud, see
[inference-debloating.md](inference-debloating.md).
