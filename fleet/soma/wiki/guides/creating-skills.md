# Creating Agent Skills

Skills are portable, reusable task procedures in the [agentskills.io](https://agentskills.io/specification) format. Write once, share across Claude Code, Cursor, Hermes, Goose, and any compatible agent.

## What Is a Skill?

A skill encodes:
- **What to do** — step-by-step procedure
- **When to use it** — trigger conditions and prerequisites
- **How to verify** — success criteria and fallback behavior

Where a tool is a single function call, a skill is a multi-step workflow. Think of it as a recipe your agent can follow reliably every time.

## Directory Structure

```
skills/
├── active/          # Skills in production
│   ├── morning-briefing/
│   │   └── SKILL.md
│   └── health-check/
│       └── SKILL.md
└── proposed/        # Under review
    └── my-new-skill/
        └── SKILL.md
```

## Step 1: Create the Directory

```bash
mkdir -p skills/proposed/my-skill-name
```

Use kebab-case for the directory name. This becomes the skill's stable identifier.

## Step 2: Write SKILL.md

Every skill is a single `SKILL.md` file with YAML frontmatter followed by markdown content.

```markdown
---
name: health-check
description: "Infrastructure health check across all MooseNet containers."
version: 1.0.0
author: moosenet-io
license: MIT
agent: sentinel
container: <fleet-host>
schedule: "*/5 * * * *"
tags: [health, infrastructure, monitoring, sentinel]
compatible_agents: [lumina, claude-code, hermes]
---

## Procedure

1. Run `sentinel_health()` MCP tool.
2. Parse the JSON response for any `ok: false` entries.
3. If all checks pass: log "All systems healthy" and exit.
4. If any checks fail: call `nexus_send(to='lumina', priority='critical', ...)` with the failure details.

## Success Criteria

- All health checks return `ok: true`.
- Execution completes within 30 seconds.
- No unhandled exceptions.

## Fallback

If `sentinel_health()` tool call fails (Terminus unreachable), send a critical alert to Lumina:
"Sentinel could not reach Terminus — manual check required."
```

## Required Frontmatter Fields

| Field | Description |
|-------|-------------|
| `name` | Kebab-case identifier. Stable — never change after activation. |
| `description` | One-line summary for discovery. |
| `version` | Semantic version (e.g., `1.0.0`). |
| `author` | Your handle or org name. |
| `agent` | Owning agent (vigil, sentinel, cortex, lumina, etc.). |
| `container` | Target container (<fleet-host>, <ironclaw-host>, etc.). |
| `tags` | List of tags for discovery and search. |

## Optional Frontmatter Fields

| Field | Description |
|-------|-------------|
| `license` | Default: MIT |
| `schedule` | Cron expression if timer-triggered |
| `compatible_agents` | Which runtimes can execute this skill |

## Step 3: Test the Skill

Ask Lumina to execute the skill by name:

```
Run the health-check skill
```

Lumina will locate the SKILL.md, follow the procedure, and report results.

## Step 4: Activate the Skill

Once verified, move from proposed to active:

```bash
mv skills/proposed/my-skill-name skills/active/my-skill-name
```

Or use Soma's **Skills** page to activate via the UI.

## Skill Discovery

Skills are auto-discovered by `skills_loader.py`:

```python
from shared.skills_loader import load_skills

skills = load_skills()          # Scans skills/active/
skill = skills['morning-briefing']
print(skill.description)
```

Terminus exposes skills via `skills_tools.py`:

| Tool | Description |
|------|-------------|
| `skills_list()` | List all active skills |
| `skills_read(name)` | Read full SKILL.md for a skill |
| `skills_create(...)` | Create a new skill (lands in proposed/) |

## Skill Evolution

Skills improve through use. When Lumina observes a better approach while executing a skill, she proposes an update. Updates require your approval before being written to `SKILL.md`.

Inspired by [SkillClaw](https://arxiv.org/abs/2604.08377) — collective skill evolution.

## Sharing Skills

Skills conforming to the agentskills.io specification can be shared at [agentskills.io](https://agentskills.io). The registry is searchable by tag, agent compatibility, and use case.

## Related

- `skills/README.md` — Skills directory overview
- [Adding MCP Tools](adding-tools.md) — Tools that skills call
- [agentskills.io specification](https://agentskills.io/specification) — Full format reference
