# AGENTS.md — Lumina Constellation (Codex)

## Your Role

You are **Terminal 3 (Codex)** in a three-terminal build setup:
- **Terminal 1** — Claude Code (OAuth session) — primary infrastructure and Soma
- **Terminal 2** — Claude Code (LiteLLM session) — fleet services and feedback loops
- **Terminal 3** — You (Codex) — assigned tasks from the operator

Read T1 and T2 progress files before starting. Write your own.

## Before You Start — Every Session

```bash
cd /home/coder/lumina-constellation
git pull gitea main                          # Always pull first
cat /home/coder/session-progress-t1.md      # What T1 is doing
cat /home/coder/session-progress-t2.md      # What T2 is doing
```

Write to `/home/coder/session-progress-t3.md` as you work.

## Coordination Protocol

**Check before touching these — another terminal may be working there:**

| Directory | Who owns it | Coordinate if... |
|-----------|-------------|-----------------|
| `fleet/soma/` | T1/T2 shared | Any Soma template or API change |
| `terminus/server.py` | T1 | Adding new tool registrations |
| `fleet/spectra/` | T1 | Spectra service changes |
| `fleet/scheduler.py` | T1 | Timer/scheduler changes |
| `deploy/` | T1 | Docker Compose or installer changes |

**Safe to work on without coordination:** `docs/`, `fleet/security/`, `fleet/myelin/`, `fleet/meridian/`, `fleet/seer/`, `fleet/cortex/`, `fleet/dura/`, `terminus/*_tools.py` (individual tool files, not server.py), `README.md`, `CITATIONS.md`.

## Commit Discipline

```bash
# After each logical unit of work:
git add [specific files]
git commit -m "clear description of what and why"
git push gitea main
```

- Commit frequently — at minimum after each completed task
- Never `git add -A` blindly — stage specific files
- Push to Gitea only; GitHub publish is a separate step run by T1

## Environment

You run on the dev host (`/home/coder/lumina-constellation`). SSH access:

```bash
ssh pvs               # PVS host → pct exec 310/305/300/315 for fleet containers
ssh terminus          # terminus-host — MCP hub (/opt/ai-mcp/)
ssh gitea             # gitea-host — Gitea
```

Fleet host (fleet-host) path: `/opt/lumina-fleet/`
Terminus (terminus-host) path: `/opt/ai-mcp/`

## Coding Standards

**Python:**
- No hardcoded IPs or `CT###` container IDs in code — use env vars or role names
- `os.environ.get('VAR_NAME', '')` not `os.environ['VAR_NAME']`
- File deploys: write locally → `scp` to pvs → `pct push CTN /tmp/file /dest/`
- Never heredocs through `pct exec` for files with complex content

**HTML / Soma templates:**
- Every template must include: `<link rel="stylesheet" href="/shared/constellation.css">`
- Every template must include: `<script src="/shared/htmx.min.js"></script>` (relative path only)
- No hardcoded hex colors — use CSS variables from constellation.css
- Templates that extend `base.html` inherit these automatically

**Git hygiene:**
- Commit messages: imperative, describe what changed and why
- No secrets, IPs, or `CT###` references in committed files
- If the pre-commit hook blocks: fix the issue, don't use `--no-verify` unless it's documented CIDR ranges in infrastructure config

## Inference De-bloat Rule

Use the minimum inference tier that works:
1. Python/template → `$0`
2. Local Qwen (via LiteLLM `Lumina Fast`) → `$0`
3. Haiku → `~$0.001`
4. Sonnet → last resort

## Module Names (Lumina naming schema)

| Module | What it does |
|--------|-------------|
| Nexus | Inter-agent inbox (Postgres-backed) |
| Terminus | MCP tool hub (terminus-host, `/opt/ai-mcp/`) |
| Engram | Memory / knowledge store |
| Soma | Admin dashboard (port 8082) |
| Axon | Work queue executor |
| Vigil | Daily briefings |
| Sentinel | Infrastructure monitoring |
| Spectra | Browser automation (Playwright) |
| Myelin | LLM cost governance |
| Synapse | Notification routing |
| Vector | Autonomous dev loops |
| Obsidian Circle | Multi-model reasoning council |

## Plane API

```bash
source /home/coder/.env
# Workspace: moosenet, base: http://YOUR_PLANE_HOST/api/v1
# Use ~/plane-helper/plane_helper.py — rate-limited, never raw curl
python3 ~/plane-helper/plane_helper.py --help
```

## Progress File Format

```markdown
# Session N — T3 Progress (Codex)
Last updated: [timestamp]

## Completed
- [item]: [result]

## Current
- [what you're working on]

## Blockers
- [anything blocked]

## T1/T2 Coordination
- server.py: [status — safe/in use]
```
