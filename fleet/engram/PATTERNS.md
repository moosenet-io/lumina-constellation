# Engram Write Patterns — Reflexa Trigger Tiers

This document describes the three-tier write pattern used by Reflexa hooks in the Lumina fleet. It is the reference for Vector (dev loop agent) and any other agent that needs to add new memory write hooks.

---

## Overview

Reflexa is the write layer of Engram (the Lumina memory system). When agents complete meaningful work, they call Reflexa hooks to persist structured memories into the SQLite-vec store on <fleet-host>. Not every event is worth storing — the tier system controls what gets written, at what cost, and how long it is retained.

---

## Tier Definitions

### T1 — Ephemeral Context (lightweight, short TTL)

**Purpose:** Capture transient working state that helps within a single session or shift. Not expected to survive more than 24–72 hours.

**What gets stored:**
- Current task focus (active Plane work item IDs)
- Recent tool call outcomes (last N results for dedup)
- Short-term agent state flags (e.g., "briefing already sent today")
- Scratch notes the agent writes to itself mid-task

**Embedding:** None. Stored as plain JSON in the `context` table.

**TTL:** 24–72 hours (configurable per agent). Auto-purged by engram.py cleanup job.

**When to trigger:** After any tool call that changes agent working state. Low friction — can be called frequently.

**Example hook location:** `vigil/briefing.py` — writes last-sent timestamp after each briefing delivery.

---

### T2 — Episodic Memory (structured, medium TTL)

**Purpose:** Record completed events that may inform future decisions. The primary tier for agent learning and deduplication across sessions.

**What gets stored:**
- Completed work items (summary, outcome, linked Plane ID)
- Anomalies observed and how they were resolved
- User preference signals extracted from interaction
- Agent-to-agent delegation outcomes (what was asked, what came back)

**Embedding:** Yes. Text is embedded via the local embedding model and stored in sqlite-vec for semantic search.

**TTL:** 30–90 days. Retention policy varies by agent — Sentinel keeps ops events 90 days, Vigil keeps briefing records 30 days.

**When to trigger:** On task completion, on anomaly resolution, on Nexus message acknowledgement. One write per meaningful event — not per tool call.

**Example hook location:** `sentinel/ops.py` — writes anomaly record after `nexus_send` confirmation of escalation.

---

### T3 — Long-Term Knowledge (persistent, no TTL)

**Purpose:** Durable facts and patterns that should survive indefinitely. The foundation of Lumina's behavioral continuity.

**What gets stored:**
- Validated behavioral rules (extracted from LUMINA.md updates)
- Infrastructure topology facts (container IPs, service locations)
- Confirmed user preferences (sourced from Lumina after operator feedback)
- Recurring pattern summaries (weekly rollups from T2 episodic records)

**Embedding:** Yes, with higher-quality re-embedding on update.

**TTL:** None. Entries are versioned, not deleted. Superseded facts are marked `retired: true` and kept for audit.

**When to trigger:** Infrequently. T3 writes are intentional, not automatic. Typically triggered by Lumina after an explicit instruction from the operator, or by a weekly rollup job.

**Example hook location:** `engram/engram.py` — `write_knowledge(fact, source, tier=3)` called directly by Lumina after operator confirmation.

---

## How to Add a New Write Hook

1. **Decide the tier.** Ask: is this transient state (T1), a completed event (T2), or a durable fact (T3)?

2. **Import the Engram helper in your agent module:**
   ```python
   import sys
   sys.path.insert(0, "/opt/lumina-fleet")
   from engram.engram import write_memory, write_context, write_knowledge
   ```

3. **Call the appropriate function after your trigger condition:**
   ```python
   # T1 — context flag
   write_context(agent="vigil", key="briefing_sent_today", value="true", ttl_hours=24)

   # T2 — episodic event
   write_memory(agent="sentinel", text="Disk usage on <fleet-host> hit 91%. Escalated via Nexus.", tags=["ops", "disk", "ct310"])

   # T3 — durable fact (use sparingly)
   write_knowledge(fact="the operator prefers briefings before 08:30 local time.", source="operator-feedback", tier=3)
   ```

4. **Test the write** by running your agent module directly on <fleet-host> and verifying the entry appears:
   ```bash
   python3 /opt/lumina-fleet/engram/engram.py query "your search term"
   ```

5. **Register your hook** in the agent's `AGENT.md` under the "Memory hooks" section so the pattern is documented.

---

## Retention and Cleanup

Engram runs a cleanup job (engram-cleanup.timer, <fleet-host> systemd) nightly at 02:00. It:
- Purges T1 records past their TTL
- Summarizes T2 records older than 30 days into weekly rollups (new T2 with `rollup: true`)
- Never touches T3

Do not bypass cleanup by setting artificially long T1 TTLs. If a record deserves to survive past 72 hours, promote it to T2.

---

## Notes for Vector (dev loop agent)

Vector hooks are primarily T1 and T2:
- **T1:** Write current task context at loop start (active branch, issue ID, last test result).
- **T2:** Write completed dev cycle summary on merge or close (what was built, test outcome, duration).
- **T3:** Only if a new architecture pattern was validated and Lumina explicitly confirms it should be retained.

Vector should never write T3 autonomously. Route through Lumina for any durable knowledge writes.
