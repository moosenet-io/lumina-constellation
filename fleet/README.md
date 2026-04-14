# Fleet — Agent Fleet

The fleet directory contains all Lumina Constellation sub-agent processes. These are the workers Lumina delegates to — each agent is purpose-built for a specific domain.

**Deploys to:** CT310 (<fleet-server-ip>) at `/opt/lumina-fleet/`
**Managed by:** systemd services per agent
**Communication:** Agents receive work via Nexus inbox. Results route back through Lumina.

For the tools agents call, see [terminus/](../terminus/README.md).

---

## Agents

| Directory | Agent | Role | Inference |
|-----------|-------|------|-----------|
| [axon/](axon/README.md) | **Axon** | Work queue manager. Reads Nexus inbox, dispatches tasks. | Pure Python |
| [vigil/](vigil/README.md) | **Vigil** | Daily briefings: weather, calendar, commute, news. | Python + local model |
| [sentinel/](sentinel/README.md) | **Sentinel** | Infrastructure health. Alerts only on failure. | Pure Python |
| [vector/](vector/) | **Vector** | Autonomous dev loops with feedback gates. | Cloud (Claude Code) |
| [seer/](seer/) | **Seer** | Multi-source research and report generation. | Cloud (tiered) |
| [cortex/](cortex/) | **Cortex** | Code intelligence: AST, blast radius, review certs. | Cloud (Sonnet) |
| [myelin/](myelin/) | **Myelin** | Token governance, cost tracking, runaway detection. | Pure Python |
| [dura/](dura/) | **Dura** | Backups, smoke tests, log aggregation, secret rotation. | Pure Python |
| [soma/](soma/) | **Soma** | Web admin panel and onboarding wizard. | FastAPI |
| [engram/](../engram/README.md) | **Engram** | Semantic memory system (shared across agents). | sqlite-vec |
| [wizard/](wizard/) | **Mr. Wizard** | Deep reasoning via the Obsidian Circle council. | Multi-model |
| [dashboard/](dashboard/) | **Dashboard** | Read-only daily view: weather, calendar, health, costs. | Pure Python |
| [shared/](shared/) | **Shared** | constellation.css, templates, agent_loader.py, docs. | — |

---

## How Agents Are Structured

Each agent directory typically contains:

```
fleet/vigil/
├── briefing.py          # Main agent script
├── briefing_dashboard.py # HTML report generator
└── vigil.service        # systemd unit file
```

Agents are stateless processes. They read input from Nexus or are triggered by Terminus MCP tools. State is stored in Engram (semantic memory) or Postgres (Nexus inbox).

---

## Agent Definitions (.agent.yaml)

Every agent has a definition file in [agents/](../agents/):

```yaml
name: vigil
display_name: "Vigil"
role: "Morning and evening briefings"
model: "local/qwen"
container: CT310
tools: [google, commute, news]
engram.namespace: agents/vigil
```

Use `agent_loader.py` to read agent definitions — never read `constellation.yaml` directly:

```python
from shared.agent_loader import display_name, get_agent
name = display_name('vigil')  # Returns "Vigil" or custom name set during naming ceremony
```

---

## How Agents Communicate

All inter-agent communication routes through **Nexus** (the inbox system). No peer-to-peer messaging.

```
the operator → Matrix → Lumina → nexus_send() → Nexus inbox
Axon  → nexus_check() → reads inbox → dispatches work
Agent → completes task → nexus_send() → Lumina → the operator
```

This architecture means any agent can be replaced, restarted, or debugged without affecting others.

---

## Adding a New Agent

1. Create the agent directory: `fleet/{name}/`
2. Write the main script: `fleet/{name}/{name}.py`
3. Create a systemd service: `fleet/{name}/{name}.service`
4. Create the agent definition: `agents/{name}.agent.yaml`
5. Verify discovery: `python3 /opt/lumina-fleet/shared/agent_loader.py`
6. Deploy to CT310 and enable the service.

---

## First-Run Setup

After deploying, run the naming ceremony to customize agent display names:

```bash
python3 /opt/lumina-fleet/naming_ceremony.py
```

This creates/updates `constellation.yaml` with your preferred agent names. The `naming.py` module auto-discovers any `.agent.yaml` files added later.

---

## Open Source Dependencies

| Module | Dependency | License | Author/Project |
|--------|-----------|---------|----------------|
| Cortex | [code-review-graph](https://github.com/tirth8205/code-review-graph) v2.2.2 | MIT | Tirth Patel — Tree-sitter AST parsing, blast radius |
| Engram | [sqlite-vec](https://github.com/asg017/sqlite-vec) | MIT/Apache | Alex Garcia — local vector embeddings |
| Gateway | [FastAPI](https://github.com/tiangolo/fastapi) | MIT | Sebastián Ramírez — async REST API |
| Vigil/Cortex | [caldav](https://github.com/python-caldav/caldav) v3.1.0 | GPL/Apache | python-caldav contributors |
| Seer | [SearXNG](https://github.com/searxng/searxng) | AGPL | SearXNG contributors |
| All agents | [psycopg2](https://github.com/psycopg/psycopg2) | LGPL | Federico Di Gregorio — PostgreSQL adapter |
| Cortex | [tree-sitter-language-pack](https://github.com/Goldziher/tree-sitter-language-pack) | MIT | Goldziher — 19-language AST support |
