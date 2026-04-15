# Frequently Asked Questions

## General

### What is Lumina Constellation?

A self-hosted, multi-agent AI personal assistant. 25 modules covering briefings, finances, kitchen, health, travel, vehicle maintenance, and more. Runs on your own hardware. Under a dollar a day.

See [What is Lumina?](../getting-started/overview.md).

### How much does it cost to run?

Under $1/day is the target for a full deployment. The key is inference de-bloating — ~90% of operations are Python at $0, ~8% use free local models, and cloud AI is reserved for the ~2% that needs genuine reasoning.

See [Inference De-Bloating](../architecture/inference-de-bloating.md).

### Does Lumina remember me between conversations?

Yes. Engram (the memory system) stores preferences, patterns, and history using local vector embeddings. Lumina loads relevant context at the start of each session. The more you use it, the better it knows you.

### Can multiple people in a household use it?

Yes. Each person gets their own agent with their own personality, calendar, and private data. Household resources (grocery lists, meal plans, travel plans) are shared via the `shared/household` Engram namespace.

Add a new agent: create one `.agent.yaml` file in `agents/`, start the container, and they're guided through a naming ceremony on first launch.

---

## Installation

### What are the hardware requirements?

- Minimum: 4 CPU cores, 8GB RAM, 50GB storage
- Recommended: 8+ cores, 16GB RAM for running local models
- Optional: NVIDIA GPU for fast local inference (Ollama)

### Can I run this on a single machine?

Yes. The Docker Compose `minimal` profile runs everything on one host. The Proxmox cluster layout in CLAUDE.md is for a production multi-node deployment.

### Do I need a GPU?

No. Local Qwen inference via Ollama works on CPU (slower). Cloud providers are the fallback for reasoning tasks. GPU adds speed and reduces latency for local models.

### What's the difference between deployment profiles?

| Profile | What you get | When to use |
|---------|-------------|-------------|
| `minimal` | Core agent + admin panel | Evaluation, low-resource hosts |
| `standard` | Everything except local inference | Most users |
| `gpu` | Full system with Ollama GPU | You have an NVIDIA GPU |
| `headless` | API-only, no admin panel | Embedding in existing infrastructure |

---

## Configuration

### How do I rename my assistant?

Run the naming ceremony:
```bash
docker exec -it lumina-fleet python3 /opt/lumina-fleet/naming_ceremony.py
```

Or use Soma's **Config** tab to rename any agent directly.

### Where are secrets stored?

Secrets are managed by [Infisical](https://infisical.com) in a self-hosted instance. They are fetched into `.env` files at runtime by `fetch-mcp-secrets.sh` — never committed to git.

For a Docker deployment without Infisical, use Docker secrets or a `.env` file that you manage manually.

### How do I add a Google Calendar?

1. Create a Google App Password (not your main password) for CalDAV access
2. In Soma > Config > Vigil, enter your CalDAV URL: `https://apidata.googleusercontent.com/caldav/v2/{email}/events/`
3. Enter your Google account email and the App Password
4. Test with: `docker exec lumina-fleet python3 /opt/lumina-fleet/vigil/briefing.py --test`

### Where is constellation.yaml?

`/opt/lumina-fleet/constellation.yaml` on <fleet-host> (or the fleet container in Docker). It stores agent display names and module configuration. Never edit it directly — use Soma's Config tab or `naming_ceremony.py`.

---

## Agents and Tools

### What's the difference between an agent, a module, and a tool?

- **Agent** — a running process with a personality and goals (Vigil, Sentinel, Axon)
- **Module** — a feature set that may include an agent, MCP tools, and/or a backend service (e.g., Nexus = Postgres backend + nexus_tools.py MCP tools + inbox-monitor routine)
- **Tool** — a single callable function exposed via MCP (e.g., `nexus_send()`)

### How does Lumina know which tools to use?

Refractor (the Smart Proxy) filters the 200+ Terminus tools to 17–28 per reasoning turn based on keyword categories. Lumina's message context determines which categories are relevant. This keeps each LLM context window lean and reduces cost.

### How do I add a new capability?

1. Write an MCP tool module in `terminus/`
2. Register in `terminus/server.py`
3. Add a Refractor keyword category
4. Optionally write a skill in `skills/proposed/`

See [Adding MCP Tools](../guides/adding-tools.md).

### Why doesn't Lumina use IronClaw's native messaging?

Because Lumina's sub-agents (Vigil, Sentinel, Axon) are Python processes, not IronClaw agents. IronClaw's `sessions_send` only works between IronClaw agent sessions. Nexus is a custom inbox that works for any process that can talk to Postgres.

---

## Troubleshooting

### Soma shows a module as red (down)

1. Check the Logs tab in Soma for that module's systemd output
2. SSH to <fleet-host> and run: `systemctl status {module}` and `journalctl -u {module} -n 50`
3. Check that the module's required env vars are set in `.env`

### Briefings stopped working

1. Check Vigil is running: `systemctl status vigil`
2. Test manually: `python3 /opt/lumina-fleet/vigil/briefing.py --test`
3. Check API keys: NewsAPI, TomTom, weather provider
4. Check Matrix bot is connected: look for errors in `journalctl -u matrix-bot`

### Nexus messages aren't being delivered

1. Check Postgres is running on <postgres-host>: `ssh root@YOUR_PVS_HOST_IP "pct exec 300 -- systemctl status postgresql"`
2. Check Nexus env vars: `INBOX_DB_HOST`, `INBOX_DB_USER`, `INBOX_DB_PASS`
3. Test with a direct psql query on <postgres-host>

### IronClaw can't find Terminus tools

1. Check Terminus is running on <terminus-host>: `ssh root@YOUR_TERMINUS_IP "systemctl status ai-mcp"`
2. Check stdio.sh is executable: `ls -la /opt/ai-mcp/stdio.sh`
3. Run `ironclaw mcp test moosenet` from <ironclaw-host> and check the output

---

## Development

### How do I contribute?

Browse the [specs/](https://github.com/moosenet-io/lumina-constellation/tree/main/specs) directory for system design context. File issues for bugs. Skills can be shared at [agentskills.io](https://agentskills.io).

### Where are the design specs?

`specs/` in the monorepo. Each major feature has a PRD (Product Requirements Document).

### What's the IronClaw version?

v0.24.0 on <ironclaw-host>. IronClaw is a Rust-based, security-first agent runtime from [NEAR AI](https://github.com/nearai/ironclaw).
