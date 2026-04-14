# Terminus — MCP Tool Hub

Terminus is the MCP (Model Context Protocol) tool hub for Lumina Constellation. It exposes all system capabilities as callable tools that Lumina and other agents use during their reasoning turns.

**Deploys to:** CT214 (<terminus-ip>) at `/opt/ai-mcp/`
**Transport:** FastMCP via stdio
**Tool count:** 20 modules, 200+ individual tools

For the agents that call these tools, see [fleet/](../fleet/README.md).

---

## Tool Modules

| File | Domain | Key tools |
|------|--------|-----------|
| `nexus_tools.py` | Inbox | nexus_send, nexus_check, nexus_read, nexus_ack, nexus_history |
| `axon_tools.py` | Work queue | axon_dispatch, axon_status, axon_complete |
| `vigil_tools.py` | Briefings | vigil_run, vigil_dashboard |
| `sentinel_tools.py` | Ops monitoring | sentinel_health, sentinel_alert, sentinel_status |
| `plane_tools.py` | Work management | 24 Plane CE CRUD tools (projects, work items, cycles) |
| `google_tools.py` | Calendar / email | CalDAV event fetch, IMAP inbox check |
| `engram_tools.py` | Memory | engram_store, engram_search, engram_recall |
| `seer_tools.py` | Research | seer_search, seer_report |
| `cortex_tools.py` | Code intelligence | cortex_blast_radius, cortex_audit, cortex_review |
| `hearth_tools.py` | Kitchen | pantry check, recipe match, shopping list |
| `ledger_tools.py` | Finance | budget summary, spending alerts, category report |
| `relay_tools.py` | Vehicle | service history, fuel log, maintenance due |
| `odyssey_tools.py` | Travel | bucket list, deal check, loyalty points |
| `crucible_tools.py` | Learning | course tracker, reading queue, streak check |
| `meridian_tools.py` | Trading | paper trade, portfolio summary, journal entry |
| `myelin_tools.py` | Cost governance | token usage, cost report, runaway check |
| `dura_tools.py` | Resilience | backup status, smoke test, log summary |
| `soma_tools.py` | Admin | module status, user config, help lookup |
| `wizard_tools.py` | Deep reasoning | council_convene, council_result |
| `server.py` | Server | FastMCP registration, tool discovery |

---

## Key Files

| File | Purpose |
|------|---------|
| `server.py` | FastMCP server entry point. Imports and registers all tool modules. |
| `fetch-mcp-secrets.sh` | Pulls secrets from Infisical (CT221) into `.env` before server start. |
| `stdio.sh` | Wrapper: sources `.env` with `set -a`, then launches `server.py --stdio`. |
| `.env` | Runtime secrets (never committed — fetched from Infisical). |

---

## How Tools Are Organized

Each tool module follows a consistent pattern:

```python
# Example: vigil_tools.py
from mcp import FastMCP

def register_vigil_tools(mcp: FastMCP):
    @mcp.tool()
    def vigil_run(...) -> str:
        ...

    @mcp.tool()
    def vigil_dashboard(...) -> str:
        ...
```

`server.py` imports and registers every module:

```python
from vigil_tools import register_vigil_tools
from sentinel_tools import register_sentinel_tools
# ... all modules

register_vigil_tools(mcp)
register_sentinel_tools(mcp)
```

---

## How to Add a New Tool Module

1. Create `yourmodule_tools.py` in this directory.
2. Define a `register_yourmodule_tools(mcp)` function containing `@mcp.tool()` decorated functions.
3. Add the import and registration call to `server.py`.
4. If new secrets are needed, add stub lines to `.env` and add them to Infisical (CT221, workspace: moosenet-services, env: prod).
5. Restart the `ai-mcp` systemd service on CT214.

---

## How IronClaw Discovers Tools

IronClaw (the agent runtime on CT305) connects to Terminus via stdio transport using `stdio.sh` as the command. The MCP protocol auto-discovers all registered tools at connection time. Refractor (Smart Proxy) then filters the 200+ tool list down to 17–28 per turn based on keyword categories, keeping each LLM context window lean.

---

## Configuration

Terminus reads secrets from `.env` via `fetch-mcp-secrets.sh` (pulls from Infisical at CT221):

| Variable | Purpose |
|----------|---------|
| `INBOX_DB_HOST` | Nexus Postgres host (CT300) |
| `INBOX_DB_USER` | Nexus database user |
| `INBOX_DB_PASS` | Nexus database password |
| `PLANE_API_TOKEN` | Plane CE API token |
| `GITEA_TOKEN` | Gitea API token |
| `GOOGLE_APP_PASSWORD` | Google CalDAV + IMAP App Password |
| `NEWS_API_KEY` | NewsAPI key |
| `GROCY_API_KEY` | Grocy server API key |
| `TOMTOM_API_KEY` | TomTom routing API key |

New secrets: add stub line to `.env`, then add to Infisical (CT221, workspace: moosenet-services, env: prod).

## History / Lineage

Terminus was originally named "ai-mcp" (the container CT214 still carries that name). The rename to Terminus was adopted in session 11 as part of the Lumina naming consolidation — "Terminus" evokes the hub at the end of all lines, where tools are reached. The tool count has grown from 9 modules (session 1) to 20+ modules and 200+ tools. The plugin loader (`plugin_loader.py`) was added in session 10 to allow drop-in tool extensions without modifying `server.py`.

## Open Source Dependencies

| Tool Module | Dependency | License | Author |
|-------------|-----------|---------|--------|
| `cortex_tools.py` | [code-review-graph](https://github.com/tirth8205/code-review-graph) v2.2.2 | MIT | Tirth Patel — Tree-sitter AST blast radius engine |
| `google_tools.py` | [caldav](https://github.com/python-caldav/caldav) v3.1.0 | GPL/Apache | python-caldav contributors |
| `nexus_tools.py`, `axon_tools.py` | [psycopg2](https://github.com/psycopg/psycopg2) | LGPL | Federico Di Gregorio |
| All FastMCP tools | [FastMCP](https://github.com/jlowin/fastmcp) | MIT | Jeremy Lowin — MCP tool registration framework |
| `server.py` | [mcp](https://github.com/modelcontextprotocol/python-sdk) | MIT | Anthropic — Model Context Protocol Python SDK |
