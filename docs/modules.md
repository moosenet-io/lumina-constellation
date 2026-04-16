# Modules

> 25 modules. Each one does one thing well.

## Brain

### Engram
Long-term memory. 391+ facts in sqlite-vec, Zettelkasten-linked, RAG-retrievable by every agent. Stores knowledge from conversations, web research, decisions, and patterns. [→ engram/](../engram/)

### Obsidian Circle
Multi-model reasoning council. Convenes multiple AI models to deliberate on hard problems. 7 presets, 7 personas, session checkpointing, synthesis by the strongest model. [→ fleet/obsidian_circle/](../fleet/obsidian_circle/)

### Mr. Wizard
Deep reasoning specialist. Lives inside Obsidian Circle config as the synthesis engine — distills positions from all council members into a final recommendation.

### Cortex
Code intelligence. Analyzes repository structure, quality metrics, and dependency graphs. Feeds into Vector's dev loops and the Obsidian Circle's code review sessions. [→ fleet/cortex/](../fleet/cortex/)

## Nervous system

### Axon
Work queue executor. Polls Plexus (Plane) for tasks, dispatches to the right agent, tracks completion. [→ fleet/axon/](../fleet/axon/)

### Myelin
Cost governance. Per-consumer virtual keys (MY.1-9), daily budget enforcement, burn rate tracking, subscription pacing. The $10/day circuit breaker. [→ fleet/myelin/](../fleet/myelin/)

### Synapse
Notification routing. Takes structured alerts from any module and delivers through Matrix, email, or webhook with appropriate urgency. [→ fleet/synapse/](../fleet/synapse/)

### Refractor
Smart request routing. 32+ categories that map incoming requests to the right tool module and inference tier. Middleware in Terminus.

## Senses

### Spectra
Browser automation. Sandboxed Playwright with 5-layer security. Live View via noVNC in Soma. Session recording via rrweb. Human-in-the-loop via Matrix. 21 MCP tools. [→ fleet/spectra/](../fleet/spectra/)

### Seer
Web research agent. SearXNG search with prompt injection defense. Decomposes queries, ranks sources, synthesizes reports. Stores findings in Engram. [→ fleet/seer/](../fleet/seer/)

### Vigil
Daily briefings. Compiles weather, traffic, calendar, news, infrastructure health at 7 AM. Delivers to Matrix. [→ fleet/vigil/](../fleet/vigil/)

### Sentinel
Infrastructure monitoring. 20 health checks every 30 minutes. Prometheus metrics. LLM runaway detection. [→ fleet/sentinel/](../fleet/sentinel/)

## Body

### Soma
The dashboard. 14+ pages behind JWT auth. Live View into Spectra, session recordings, config panels, wiki, module status. [→ fleet/soma/](../fleet/soma/)

### Nexus
The inbox. Every notification, alert, and work order flows through Nexus before routing. Postgres-backed. [→ fleet/nexus/](../fleet/nexus/)

### Plexus
Work queue backed by Plane CE. 850+ items tracked, 24 CRUD operations via plane_tools.py in Terminus.

### Terminus
The MCP tool hub. 38 modules, 272+ tools. Every agent connects here. [→ terminus/](../terminus/)

### Dura
Operational resilience. Secret rotation, backup orchestration, smoke testing, disaster recovery. [→ fleet/dura/](../fleet/dura/)

## Life

### Vector
Development loops with Calx behavioral correction. Autonomous coding, testing, iteration. [→ fleet/vector/](../fleet/vector/)

### Meridian
Paper trading sandbox. Virtual portfolio, real market data, reasoning journal. Never touches real money. [→ fleet/meridian/](../fleet/meridian/)

### Odyssey
Travel planning. *Spec'd, not yet built.*

### Vitals
Health tracking. *Spec'd, not yet built.*

### Hearth
Household management. *Spec'd, not yet built.*

### Ledger
Expense tracking with Actual Budget integration. *Partially built.*

### Relay
Vehicle management with LubeLogger integration. *Partially built.*

## Identity

### Lumina
The orchestrator. Personality-first AI agent running on IronClaw. Has opinions, memory, a daily schedule. Communicates via Matrix. [→ agents/](../agents/)

### Lumière
Partner agent. Binary running, persona and MCP connections pending. Awaiting naming ceremony. [→ agents/](../agents/)
