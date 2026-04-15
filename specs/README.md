# Specs — System Design Specifications

This directory holds the specification documents that define how Lumina Constellation is built. Specs are the authoritative source of truth for system design decisions and drive Plane work items.

---

## What Specs Are

A spec is a detailed design document written before implementation begins. It answers:
- What problem does this solve?
- How does it work, end to end?
- What are the data structures and APIs?
- What are the failure modes and edge cases?
- How is it tested?

Specs are written in `.docx` format for rich editing during planning, then referenced during build sessions via Claude Code.

---

## Current Specs

All 32 spec documents are in this directory. See [INDEX.md](INDEX.md) for the full inventory with descriptions.

Key active specs:

| File | Description | Plane project |
|------|-------------|--------------|
| [lumina-nexus-prd.docx](lumina-nexus-prd.docx) | Nexus inter-agent inbox system PRD. Phases 1-4: Postgres backend, MCP tools, Lumina integration, Axon work queue. | LM |
| [lumina-session12-build-spec.docx](lumina-session12-build-spec.docx) | Session 12 build spec. Security sprint, README audit, OpenRouter routing fix. | LM |
| [lumina-session11-build-spec.docx](lumina-session11-build-spec.docx) | Session 11 build spec. Monorepo consolidation, module smoketests, MCP tool verification. | LM |
| [lumina-soma-prd.docx](lumina-soma-prd.docx) | Soma admin panel PRD. | LM |
| [lumina-vector-enhancement-spec.docx](lumina-vector-enhancement-spec.docx) | Vector enhancement — Calx behavioral correction, skill evolution. | LM |

---

## How Specs Drive Work

1. the operator writes or dictates a spec (PRD or build spec format).
2. The spec is added here and linked in the relevant Plane project.
3. During a Claude Code build session, the spec is read at the start and used to guide implementation.
4. Completed phases are checked off in the spec document.
5. Implementation notes and deviations are appended to the spec after the session.

---

## Spec Naming Convention

| Type | Format | Example |
|------|--------|---------|
| PRD (new system) | `lumina-{system}-prd.docx` | `lumina-nexus-prd.docx` |
| Build spec (session) | `lumina-session{N}-build-spec.docx` | `lumina-session11-build-spec.docx` |
| Module spec | `lumina-{module}-spec.docx` | `lumina-cortex-spec.docx` |

---

## History / Lineage

The specs directory was established in session 11 as the formal home for `.docx` specification documents. Prior to this, specs were stored informally and referenced verbally in build sessions. The session 11 consolidation began bringing specs into the repository for version control and direct access during Claude Code sessions.

Session 12 completed the full migration: all 32 historical spec documents (sessions 1-12, all module specs, predecessor ARCADE specs) are now in this directory. The repository is now the single source of truth — no external file system mounting required.

## Credits

- Specification format — internal Lumina PRD template (operator-authored, session 8+)
- Plane integration — Plane CE at <plane-host>; workspace moosenet; project identifiers LM (Lumina) and PX (The Plexus)

## Related

- [Root README](../README.md) — System architecture overview
- [agents/README.md](../agents/README.md) — Agent definition format
- Plane CE at http://<plane-ip>:8000 — Work items linked to spec phases
