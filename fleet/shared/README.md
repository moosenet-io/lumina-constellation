# Shared — Fleet Shared Library

Shared runtime utilities, design system, template engine, and agent loader used by every module in the Lumina fleet. Nothing in `shared/` makes inference calls — it is pure Python infrastructure.

**Deploys to:** <fleet-host> (`<fleet-server-ip>`) at `/opt/lumina-fleet/shared/`
**Inference cost:** $0 (no LLM calls)

---

## What It Does

The shared library provides the cross-cutting concerns that every fleet module needs:

- **Agent identity** — `agent_loader.py` is the single source of truth for agent names and configs. All code reads agent definitions through it; nothing reads `constellation.yaml` directly.
- **Design system** — `constellation.css` is the one stylesheet all HTML output must use. Auto light/dark mode. No inline styles permitted anywhere.
- **Templates** — `template_engine.py` renders YAML template files into notification strings, replacing LLM generation for routine messages.
- **Skills discovery** — `skills_loader.py` scans `skills/active/` at startup and exposes the SKILL.md library to agents.
- **Skill tracking** — `skill_tracker.py` records skill execution outcomes, feeding the evolution pipeline.
- **Docs generation** — `docs_generator.py` reads `.agent.yaml` files and module docstrings to produce the reference section of the help system automatically.

---

## Architecture

- **Runs on:** <fleet-host>, imported by every fleet module as `from shared.X import Y`
- **Dependencies:** Python 3.11+ standard library only (no external packages)
- **Connections:** Reads `agents/` directory for `.agent.yaml` definitions; reads `skills/` for SKILL.md files; writes `docs/index.md` (docs_generator only)

---

## Configuration

No external configuration file. Behavior is controlled by the calling module:

| Import | What it provides | Default behavior |
|--------|-----------------|-----------------|
| `agent_loader.display_name(name)` | User-visible agent name | Falls back to `name` if no ceremony override |
| `agent_loader.get_agent(name)` | Full agent config dict | Reads `agents/{name}.agent.yaml` |
| `agent_loader.load_agents()` | All agent configs | Scans `agents/` directory |
| `template_engine.render(template, vars)` | Rendered notification string | Raises on missing required vars |
| `skills_loader.load_skills()` | Dict of active skills | Scans `skills/active/` for SKILL.md |

---

## MCP Tools

Shared itself has no MCP tools. The modules it supports are exposed via Terminus tool files. Skills are accessible via `skills_tools.py` on <terminus-host>.

---

## Files

| File | Purpose |
|------|---------|
| `agent_loader.py` | Load and resolve agent definitions from `.agent.yaml` files. **Always use this instead of reading YAML directly.** |
| `constellation.css` | Unified design system for all fleet HTML output. Required in every HTML template. |
| `constellation-theme.yaml` | CSS variable definitions and theme tokens for the design system. |
| `template_engine.py` | Render YAML template files with variable substitution. Powers all notification text generation. |
| `skills_loader.py` | Discover and parse SKILL.md files from `skills/active/`. Used at agent startup. |
| `skill_tracker.py` | Record skill execution outcomes for the evolution pipeline. |
| `docs_generator.py` | Auto-generate `docs/index.md` from agent definitions and module docstrings. |
| `LUMIERE.md` | Runtime context document for Lumière (partner agent). Sourced by <partner-host> IronClaw at startup. |
| `templates/` | YAML template libraries for each module (briefings, alerts, coaching, reminders). |

### Templates directory

| File | Used by |
|------|---------|
| `templates/vigil_notifications.yaml` | Vigil — briefing delivery messages |
| `templates/dashboard_insights.yaml` | Dashboard — daily insight rotation |
| `templates/meridian_alerts.yaml` | Meridian — trading alerts |
| `templates/vitals_coaching.yaml` | Vitals — health coaching nudges |
| `templates/ledger_alerts.yaml` | Ledger — budget threshold messages |
| `templates/relay_reminders.yaml` | Relay — vehicle maintenance reminders |

---

## History / Lineage

The shared library grew organically as common patterns were extracted from fleet agents during the session 10-11 consolidation. `constellation.css` was introduced in session 11 to enforce visual consistency across all HTML-generating modules. `agent_loader.py` replaced direct YAML reads after Lumière was added as a second agent and naming ceremony overrides needed a single resolution path. The template library was built to eliminate LLM calls for routine notifications (inference de-bloating initiative, session 10).

---

## Credits

- `constellation.css` design tokens and auto-dark mode approach — influenced by GitHub Primer and Tailwind CSS variable conventions.
- Template library concept — Python `string.Template` / Jinja2 patterns, zero external dependencies.
- Agent YAML format — influenced by [NPCSH](https://github.com/NPC-Worldwide/npcsh) agent definition conventions.
