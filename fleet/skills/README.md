# Skills — Agent Skills Library

The skills library contains reusable, validated procedures that agents execute during their work loops. Skills are YAML files following the [agentskills.io](https://agentskills.io) format. They encode patterns that have proven reliable, saving inference cost by replacing LLM trial-and-error with known-good steps.

**Location:** `skills/active/` (active), `skills/proposed/` (awaiting review), `skills/disabled/` (archived)
**Deploys to:** `<fleet-host>` at `/opt/lumina-fleet/skills/`
**Cost:** $0 — skill files are read as context, no inference to load them

---

## What Skills Are

A skill is a short YAML document (SKILL.md format) that describes:
- What pattern or procedure it encodes
- When to use it (trigger conditions)
- Step-by-step instructions
- Known pitfalls to avoid
- Success criteria

Vector reads active skills during the Plan phase and injects relevant ones into the loop context. This means Vector re-uses previously solved patterns without re-discovering them.

---

## Active Skills

| Skill | Description | Used by |
|-------|-------------|---------|
| `code-review` | Code review checklist — diff analysis, test coverage, style | Vector, Cortex |
| `health-check` | System health verification steps | Sentinel, Vector |
| `morning-briefing` | Briefing assembly and delivery procedure | Vigil |

---

## Skill Format

Each skill is a SKILL.md file with frontmatter:

```yaml
---
name: add-fastmcp-tool
description: How to add a new tool to the Terminus FastMCP server
triggers:
  - "add.*tool"
  - "mcp.*tool"
pitfalls:
  - count: 0
  - items: []
success_criteria: "Tool registered in server.py and returns valid output"
---

## Steps

1. Create `yourmodule_tools.py` in `terminus/`
2. Define `register_yourmodule_tools(mcp: FastMCP)` function
3. Add `@mcp.tool()` decorator to each tool function
4. Import and register in `server.py`
5. Restart `ai-mcp` service
```

---

## Skill Evolution

Skills are created in two ways:

1. **Manual** — the operator writes and approves a skill via Soma (`/skills` page)
2. **Automatic** — Calx (Vector's behavioral correction system) detects a pattern recurring 3+ times across loop iterations and proposes promoting it to a skill

Proposed skills appear in `skills/proposed/` and show in the Soma Skills page for operator review. Approved skills move to `skills/active/`. Rejected skills are deleted.

The skill evolution timer runs at 2:00 AM alongside the project-ideas pipeline.

---

## MCP Tools (via Terminus)

| Tool | Description |
|------|-------------|
| `skill_list` | List all active skills with metadata |
| `skill_get` | Return full content of a specific skill |
| `skill_propose` | Create a new proposed skill |
| `skill_approve` | Approve a proposed skill (move to active) |

Defined in `terminus/skills_tools.py`.

---

## History / Lineage

The skills system was formalized in session 10 as part of the inference de-bloating initiative. Before skills, Vector re-solved the same patterns repeatedly across loops. The agentskills.io YAML format was adopted for compatibility with the broader agent ecosystem. Skill evolution (auto-promotion from Calx triggers) was designed in session 11.

---

## Credits

- [agentskills.io](https://agentskills.io) — SKILL.md format specification
- Calx behavioral correction — adapted from getcalx/oss (archived)
