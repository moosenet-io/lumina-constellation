# Master Execution Plan — Full Sprint

SCP all files to CT212, then paste this single command block into Claude Code.

---

## Document Inventory (SCP all to /home/coder/ on CT212)

### Specs to execute (in priority order):

| # | File | What it does | Plane Issues |
|---|------|-------------|-------------|
| 1 | security-full-sprint.md | FIRST: recreate GitHub repo, README audit, PII gate (3 layers), Gitea/GitHub dev/prod workflow, key rotation | SEC.14-20 |
| 2 | security-rotation-spec.md | Key rotation system: secrets registry, rotation engine, auto/manual methods, rollback, Sentinel integration, Soma Security tab | SEC.1-13 |
| 3 | spec-addenda-late-session.md | Fix OpenRouter Sonnet routing leak, Google Calendar robustness, Setup disclaimer+signature, project-ideas skill | 4 items |
| 4 | lumina-soma-prd.docx | Document 24: Full Soma PRD — caching layer, auth/login, all 10 pages specced, error handling, data viz, Cloudflare/OpenRouter design standard | SP.1-16 |
| 5 | lumina-vector-pulse-spec.docx | Document 24b: Vector management interface (7 sections, 19 API endpoints) + Pulse temporal awareness ($0.002/day) | SP.V1-7, SP.C1-4 |
| 6 | lumina-obsidian-circle-v3.docx | Document 25: Obsidian Circle v3 — tool-augmented deliberation, variable circles, Prism mode, structured output, module integration | OC.1-10 |
| 7 | lumina-synapse-spec.docx | Document 26: Synapse — memory-driven spontaneous conversation, 3-stage pipeline, configurable boundaries | SY.1-10 |

### Reference docs (don't execute, but Claude Code should read for context):

| File | Contents |
|------|----------|
| lumina-activity-log-apr5-14.docx | 10-day activity log, current system state |
| lumina-soma-build-spec.docx | Original Soma build spec (already executed) |
| lumina-soma-addendum.docx | Soma addendum (already executed) |
| lumina-vector-enhancement-spec.docx | Vector ARCADE gaps (already executed) |

### Already completed (for reference only):

| File | Status |
|------|--------|
| lumina-session11-build-spec.docx | DONE — 43/43 items |
| lumina-session12-build-spec.docx | DONE — all items |
| immediate-tasks-prompt.md | DONE — citations, Sentinel, Soma fix |
| soma-fix-vector-gaps-citations.md | DONE — 8 Vector gaps, citations, Soma fix |
| soma-ux-overhaul.md | DONE — partial (auth headers, wizard nav) |
| overnight-soma-engram-prompt.md | DONE — Engram ENG-65-72 |

---

## Total New Plane Issues: ~80

| Group | Issues | Priority |
|-------|--------|----------|
| Security (SEC.1-20) | 20 | HIGH — do first |
| Soma PRD (SP.1-16) | 16 | HIGH |
| Vector + Pulse (SP.V1-7, SP.C1-4) | 11 | HIGH |
| Obsidian Circle (OC.1-10) | 10 | MEDIUM |
| Synapse (SY.1-10) | 10 | MEDIUM |
| Addenda (routing fix, gcal, disclaimer, project-ideas) | 4 | MIXED |
| README audit | 1 (large) | HIGH |
| OpenRouter routing fix | 1 | HIGH |

---

## SCP Command

From your local machine (or wherever the files are):

```bash
# SCP all specs to CT212
scp -P 2222 \
  security-full-sprint.md \
  security-rotation-spec.md \
  security-repo-recreate-pii-gate.md \
  spec-addenda-late-session.md \
  lumina-soma-prd.docx \
  lumina-vector-pulse-spec.docx \
  lumina-obsidian-circle-v3.docx \
  lumina-synapse-spec.docx \
  lumina-activity-log-apr5-14.docx \
  moosenet@git.moosenet.online:/tmp/

# Then from PVM, push into CT212
ssh pvm "for f in /tmp/security-full-sprint.md /tmp/security-rotation-spec.md /tmp/security-repo-recreate-pii-gate.md /tmp/spec-addenda-late-session.md /tmp/lumina-soma-prd.docx /tmp/lumina-vector-pulse-spec.docx /tmp/lumina-obsidian-circle-v3.docx /tmp/lumina-synapse-spec.docx /tmp/lumina-activity-log-apr5-14.docx; do pct push 212 \$f /home/coder/\$(basename \$f); done"
```

---

## Master Claude Code Prompt

Paste this into Claude Code on CT212 when rate limit lifts:

```
Read the CLAUDE.md. This is a major sprint with 7 spec documents. Execute in the order below. Use the throttled Plane helper for ALL Plane API calls. Push to Gitea only (not GitHub — we have a new dev/prod workflow).

PHASE 1 — SECURITY (do first, blocks everything else):
Read /home/coder/security-full-sprint.md. Execute all 5 tasks:
1. Recreate GitHub repo from clean squash (repo is already deleted — just create fresh)
2. Audit and enhance ALL sub-READMEs (15 directories)
3. Build PII gate on 3 layers (CT214 MCP, CT212 Claude Code pre-push, operator config)
4. Set up Gitea as source of truth, GitHub as prod mirror, publish script
5. Rotate all keys via Ansible playbook

PHASE 2 — OPENROUTER FIX (urgent cost leak):
Read /home/coder/spec-addenda-late-session.md, Task 1 only.
Find what's still routing Sonnet through OpenRouter at $1.86/day. Fix the LiteLLM config.

PHASE 3 — SOMA PRD (biggest feature block):
Read /home/coder/lumina-soma-prd.docx. Execute SP.1 through SP.16:
- SP.1: Caching layer (with salted HMAC security, refresh scripts)
- SP.2: Fix all broken API endpoints
- SP.3: Rename Wizard to Setup
- SP.4: Add Wiki to sidebar
- SP.5-8: Fix Status, Config, Skills, Plugins pages
- SP.9-11: Build Sessions, Timers, Logs pages
- SP.12: Overhaul Setup flow (disclaimer, validation, partner agents, Tailscale)
- SP.13: Build Vector page (full 5-tab interface)
- SP.14: Enhance Wiki (3-panel, search, navigation)
- SP.15: Error handling standard (no blank pages, no infinite Loading)
- SP.16: Push and verify all pages
Brand everything as "Lumina" not "Soma" in the UI. Use Chart.js for data visualizations. Build login system with JWT cookies.

PHASE 4 — VECTOR + PULSE + OBSIDIAN CIRCLE:
Read /home/coder/lumina-vector-pulse-spec.docx. Execute SP.V1-V7 and SP.C1-C4:
- Vector: live loop visualization, OAuth management, Plane dashboard, enhanced task submission, history/Calx/PRs/models/config tabs
- Pulse: time provider, marker store, MCP tools, agent integration (15 tokens default injection)
- Council: split-pane viewer with Prism mode (dynamic model selection, adaptive layout)

Then read /home/coder/lumina-obsidian-circle-v3.docx. Execute OC.1-OC.10:
- convene() engine with ReAct tool-augmented deliberation
- Circle presets (quick, architecture, security, cost, research, full)
- Prism personas (Architect through Devil's Advocate, custom, mixed mode)
- Structured output validation with confidence thresholds
- Session checkpointing via Engram
- Module integration (Vector, Vigil, Sentinel, Seer, Crucible, Meridian, Odyssey)
- Soma /council page
- MCP tools + CLI

PHASE 5 — SYNAPSE + REMAINING ADDENDA:
Read /home/coder/lumina-synapse-spec.docx. Execute SY.1-SY.10:
- Trigger scanner (Engram, Pulse, Sentinel, Vector, Vigil, Zettelkasten graph)
- Relevance gate (threshold, blocklist, quiet hours, rate limit)
- Message composer (local Qwen, $0)
- Timer, Soma config UI, history page, feedback loop, MCP tools, Engram deep ties

Then read /home/coder/spec-addenda-late-session.md, Tasks 2-4:
- Google Calendar robustness wrapper
- Setup disclaimer with signature file
- Project-ideas evolving skill (self-directed pipeline)

Read /home/coder/security-rotation-spec.md. Execute SEC.1-SEC.13:
- Secrets registry, rotation engine, auto/manual methods, rollback, Sentinel integration, Soma Security tab, CLI, daily timer

PHASE 6 — FINAL:
- Run full PII scan on the repo before any push
- Push to Gitea: "Major sprint: security, Soma PRD, Vector+Pulse, Obsidian Circle v3, Synapse, key rotation"
- Run deploy/publish-to-github.sh to publish to GitHub after review
- Write /home/coder/session-major-sprint-report.md with everything completed
- Update Plane: mark all completed items Done

Estimated scope: ~80 Plane items across 6 phases. Work through as much as possible. If you hit rate limits on external services, move to the next phase and come back. Priority: security first, then Soma (most visible), then everything else.
```
