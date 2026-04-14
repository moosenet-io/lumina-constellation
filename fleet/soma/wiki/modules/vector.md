# Vector — Autonomous Dev Agent

Vector runs autonomous development loops on CT310. It receives a spec or task via Nexus, writes code, runs tests, iterates based on feedback, and commits when tests pass.

**Deploys to:** CT310 at `/opt/lumina-fleet/vector/`
**Trigger:** Nexus work order from Lumina or Axon
**Inference cost:** Cloud (Claude Code / Sonnet) — Vector tasks are genuine reasoning work

## What Vector Does

1. Receives a development task via Nexus (spec, bug description, or feature request)
2. Reads relevant code context using file tools
3. Writes or edits code
4. Runs tests and linters
5. If tests pass: commits and sends result to Lumina via Nexus
6. If tests fail: iterates with a feedback gate (max iterations configurable)

Vector uses a feedback gate to prevent infinite loops. If it can't pass tests within the configured limit, it escalates to Lumina rather than committing broken code.

## Feedback Gate Pattern

Inspired by the Ralph Loop pattern (Geoffrey Huntley):

```
spec → write → test → pass? → commit
                    ↓ fail
                  iterate (max N times)
                    ↓ still failing
                  escalate to Lumina
```

## Files

| File | Purpose |
|------|---------|
| `vector.py` | Main dev loop. Task parsing, code generation, test runner, commit logic. |
| `vector.service` | systemd service unit. |

## Task Format

```json
{
  "to": "vector",
  "from": "lumina",
  "priority": "normal",
  "task_type": "dev_task",
  "payload": {
    "description": "Add rate limiting to Plane API calls in plane_tools.py",
    "repo": "/opt/lumina-fleet",
    "test_command": "python3 -m pytest terminus/tests/",
    "max_iterations": 5
  }
}
```

## MCP Tools

Vector uses Claude Code (itself) as its inference engine. It calls Terminus tools for file operations, git operations, and test execution — but its reasoning loop runs through the Claude Code agent runtime.

## Related

- [Nexus](nexus.md) — how Vector receives work
- [Axon](axon.md) — work queue manager that dispatches to Vector
- ARCADE source: `~/arcade/` on CT212 (legacy, being consolidated)
