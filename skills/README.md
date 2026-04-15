# Skills — Agent Skills

Agent skills in the [agentskills.io](https://agentskills.io/specification) open standard format. Skills are portable, shareable task procedures compatible with Claude Code, Cursor, Hermes, Goose, and other agents.

---

## What Is a Skill?

A skill is a reusable, self-contained task procedure. Where a tool is a single function call, a skill is a multi-step workflow — the kind of thing you might explain to a new team member once and expect them to do reliably every time.

Skills encode:
- **What to do** — step-by-step procedure
- **When to use it** — trigger conditions and prerequisites
- **How to verify** — success criteria and fallback behavior

---

## Directory Structure

```
skills/
├── active/          # Skills in production use
│   ├── morning-briefing/
│   │   └── SKILL.md
│   ├── health-check/
│   │   └── SKILL.md
│   └── code-review/
│       └── SKILL.md
├── proposed/        # Skills under review before activation
└── README.md
```

---

## Current Skills

| Skill | Agent | Description |
|-------|-------|-------------|
| [morning-briefing](active/morning-briefing/SKILL.md) | Vigil | Daily briefing with weather, calendar, commute, news |
| [health-check](active/health-check/SKILL.md) | Sentinel | Infrastructure health across all MooseNet containers |
| [code-review](active/code-review/SKILL.md) | Cortex | AST-based code review with optional Obsidian Circle council |

---

## SKILL.md Frontmatter Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Kebab-case identifier — stable, used in code |
| `description` | Yes | One-line summary for discovery |
| `version` | Yes | Semantic version (e.g. `1.0.0`) |
| `author` | Yes | Skill author handle |
| `license` | No | License (default: MIT) |
| `agent` | Yes | Owning agent (vigil, sentinel, cortex, lumina, etc.) |
| `container` | Yes | Target container (<fleet-host>, etc.) |
| `schedule` | No | Cron expression or human schedule if timer-triggered |
| `tags` | Yes | List of tags for discovery and search |
| `compatible_agents` | No | Which agent runtimes can execute this skill |

### Example SKILL.md

```markdown
---
name: morning-briefing
description: "Daily briefing with weather, calendar, commute, and news."
version: 1.2.0
author: moosenet-io
license: MIT
agent: vigil
container: <fleet-host>
schedule: "0 7 * * *"
tags: [briefing, daily, weather, calendar, news]
compatible_agents: [lumina, claude-code, hermes]
---

## Procedure

1. Fetch weather forecast for configured location.
2. Pull today's calendar events via CalDAV.
3. Check commute time vs. baseline via TomTom.
4. Fetch top headlines from NewsAPI and GNews.
5. Assemble summary using briefing template.
6. Generate HTML dashboard via briefing_dashboard.py.
7. Send Matrix message via Lumina.

## Success Criteria

- Briefing delivered by 07:15.
- All data sources responded (or fallback text used for failed sources).
- HTML dashboard written to /var/www/html/briefing.html.

## Fallback

If >2 data sources fail, send a minimal briefing with available data and flag the failures.
```

---

## How Skills Are Discovered

Skills are auto-discovered by `skills_loader.py` at agent startup:

```python
# /opt/lumina-fleet/shared/skills_loader.py
from skills_loader import load_skills

skills = load_skills()          # Scans skills/active/, parses frontmatter
skill = skills['morning-briefing']
```

Terminus exposes skills via MCP tools (`skills_tools.py` on <terminus-host>):

| Tool | Description |
|------|-------------|
| `skills_list()` | List all active skills with metadata |
| `skills_read(skill_name)` | Read full SKILL.md for a skill |
| `skills_create(...)` | Create a new skill (lands in proposed/ by default) |

---

## How to Create a New Skill

1. Create a directory: `skills/proposed/{skill-name}/`
2. Write `SKILL.md` with required frontmatter and a clear procedure.
3. Test by asking Lumina to execute the skill by name.
4. When verified, move from `proposed/` to `active/`.
5. (Optional) Share at [agentskills.io](https://agentskills.io).

---

## Skill Evolution

Skills improve through use. When Lumina observes a better approach while executing a skill, she proposes an update. Updates require human approval before being written to `SKILL.md`.

Inspired by the [SkillClaw](https://arxiv.org/abs/2604.08377) collective skill evolution framework — skills improve as agents share observations across the community.

Evolution process:
1. Agent executes skill, notes a deviation or improvement opportunity.
2. Proposed change is stored in Engram as a candidate update.
3. Lumina presents the candidate to the user for approval.
4. Approved changes are committed to `SKILL.md` via a git commit.

---

## Sharing Skills

Skills conforming to the agentskills.io specification can be shared at [agentskills.io](https://agentskills.io). The registry is searchable by tag, agent compatibility, and use case.

To submit a skill:
1. Ensure `SKILL.md` has all required frontmatter fields.
2. Verify the skill is in `active/` (not `proposed/`).
3. Submit via the [agentskills.io](https://agentskills.io) web interface or API.

---

## Related

- [Root README](../README.md) — System overview, Agent Skills section
- [agents/README.md](../agents/README.md) — Agent definitions (skills are scoped per agent)
- [fleet/shared/](../fleet/shared/) — Shared loader and template libraries
- [agentskills.io specification](https://agentskills.io/specification) — Full format reference
