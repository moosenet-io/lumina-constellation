# Vector

> Autonomous dev loops for Lumina Constellation. Successor to [ARCADE](https://github.com/moosenet-io/arcade) (archived).

## What it does

Vector runs structured, self-correcting development loops inside the Lumina homelab. Given a task, it plans a set of incremental chunks, executes each one via Claude Code, evaluates outcomes, and iterates until the task is done or a cost gate fires. It integrates with the Nexus inbox so Lumina can delegate coding work and receive results without polling.

---

## Quick Start — Standalone

No Lumina infrastructure required. Useful for isolated dev work.

```bash
# 1. Clone and configure
git clone http://git.moosenet.online/moosenet/lumina-fleet
cd lumina-fleet/vector

cp config/vector.yaml.example config/vector.yaml
# Edit vector.yaml: set llm.endpoint, llm.api_key_env, and standalone.* paths

# 2. Set your LLM key
export LITELLM_MASTER_KEY=sk-...

# 3. Run
python3 vector.py --config config/vector.yaml --task "Add unit tests to payments module"
```

In standalone mode Vector writes state to a local SQLite DB and logs to `vector.log`. No Postgres or Plane connection is needed.

---

## Quick Start — Integrated (Lumina)

In integrated mode, Lumina sends work orders via the Nexus inbox and Vector reports results back.

**Send a work order from Lumina (via MCP tool):**

```
vector_submit(project="my-project", task="Refactor auth module to use JWT", priority="normal")
```

**Work order payload format (Nexus inbox_messages table):**

```json
{
  "from_agent": "lumina",
  "to_agent": "vector",
  "message_type": "work_order",
  "payload": {
    "project": "my-project",
    "task": "Refactor auth module to use JWT",
    "priority": "normal"
  }
}
```

Vector polls Nexus for pending work orders, claims them, executes the loop, and inserts a `work_result` reply message when done.

**Prerequisites for integrated mode:**

- Postgres `lumina_inbox` schema applied on <postgres-host>
- `INBOX_DB_HOST`, `INBOX_DB_USER`, `INBOX_DB_PASS` set in <terminus-host> `.env` and <fleet-host> environment
- `vector.yaml` `mode: integrated` with nexus, plane, and engram sections filled in

---

## The Loop

Vector runs a four-phase cycle for each task chunk:

1. **Plan** — Decompose the task into a prioritized queue of chunks. Each chunk is a unit of work completable in one Claude Code session. Skills are consulted to bias the plan toward known-good patterns.

2. **Execute** — Launch Claude Code (or OpenHands for scaffold tasks) against the current chunk. The loop injects CONTEXT.md, open issues, and Calx corrections as session context.

3. **Review** — Evaluate the result: did tests pass? Did the promise token appear? Did Calx fire any triggers? Log the outcome to run-log.md.

4. **Iterate** — If the chunk succeeded, advance the queue. If it failed, increment the failure counter. After three consecutive failures on the same chunk, Vector creates a Nexus escalation message and halts.

Cost is tracked per chunk. If cumulative spend exceeds `max_cost_per_run` (standalone) or `cost_gate.daily_auto_limit` (integrated), Vector pauses and requests approval via Nexus before continuing.

---

## Behavioral Correction (Calx)

> Behavioral correction concepts adapted from [getcalx/oss](https://github.com/getcalx/oss) (archived).

Calx is a lightweight trigger system that watches diffs and test results for anti-patterns, injecting corrections into the next loop iteration rather than hard-blocking (except for security violations).

Three trigger tiers:

| Tier | Type | Example | Response |
|------|------|---------|----------|
| T1 | Test compliance | New function with no test | Soft correction injected |
| T2 | Style conventions | File exceeds 500 lines | Soft correction injected |
| T3 | Security/anti-pattern | Hardcoded API key in diff | Hard block — task halted |

Calx keeps a local SQLite history (`~/.vector/calx.db`). When the same trigger fires three or more times across iterations, Calx proposes promoting the pattern to a reusable skill so future loops avoid the anti-pattern proactively.

---

## Skill-Aware Planning

Vector maintains a skill library — short YAML files describing repeatable patterns ("how to add a FastMCP tool", "how to write a psycopg2 query", etc.). During the Plan phase, the planner checks whether any registered skills apply to the current task and, if so, incorporates their steps directly into the chunk plan.

Skills live in `vector/skills/` and can be added manually or promoted automatically when Calx detects a recurring correction pattern (see `calx.skill_evolution` in config).

This reduces iteration count on common tasks and cuts cost by avoiding re-learning patterns the loop has already solved.

---

## Configuration

Key fields in `vector.yaml`:

| Field | Description |
|-------|-------------|
| `mode` | `standalone` or `integrated` |
| `llm.provider` | `litellm`, `openrouter`, `ollama`, or `anthropic` |
| `llm.model` | Model identifier passed to the provider |
| `standalone.max_cost_per_run` | Hard spend cap per invocation (USD) |
| `standalone.max_iterations` | Max loop iterations before auto-halt |
| `integrated.nexus.agent_id` | How Vector identifies itself in the inbox (default: `vector`) |
| `integrated.cost_gate.daily_auto_limit` | Auto-approve up to this amount per day; prompt via Nexus above it |
| `calx.enabled` | Enable/disable behavioral correction entirely |
| `calx.t3.block_hardcoded_secrets` | Hard block on secrets in diffs (recommended: `true`) |
| `calx.skill_evolution.min_trigger_count` | Iterations before a correction becomes a skill proposal |

See `config/vector.yaml.example` for the full annotated reference.

---

## Lineage

Vector is the direct successor to ARCADE (`github.com/moosenet-io/arcade`, archived April 2025). The core loop architecture, masterarcade.sh runner, and Gitea-based project queue format are carried forward; the codebase has been consolidated into `lumina-fleet/vector/` and the agent name updated throughout.

Behavioral correction concepts (the Calx trigger system) are adapted from `getcalx/oss` (archived). The T1/T2/T3 tier model and skill-evolution promotion logic originate there.

The public ARCADE repository (`github.com/moosenet-io/arcade`) is archived and should not be used for new work. All active development continues here.
