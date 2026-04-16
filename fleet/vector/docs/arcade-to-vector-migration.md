# ARCADE → Vector Migration Guide

This guide covers migrating from the legacy ARCADE dev loop to Vector (the Lumina-integrated autonomous dev agent).

## What Changed

| | ARCADE | Vector |
|-|--------|--------|
| Name | ARCADE | Vector |
| Identity | `_dn("arcade")` / constellation | `_dn("vector")` / constellation |
| Deployment | <fleet-host> `/opt/lumina-fleet/vector/` | Same path (in-place) |
| Modes | Single (standalone) | Standalone + Integrated |
| Task state | SQLite | SQLite (standalone) or Plane PX (integrated) |
| Messaging | Stdout / Matrix | Stdout (standalone) or Nexus (integrated) |
| Memory | Reflexa hooks | Engram MemoryStore |
| MCP tools | `arcade_*` functions | `vector_*` functions |
| GitHub repo | `moosenet-io/arcade` | `moosenet-io/vector` |

## Naming Updates

The constellation identity system handles display names automatically. No hardcoded changes needed in calling code — use `_dn("vector")` in Vector's own code.

## Tool Name Changes (<terminus-host>)

| Old MCP Tool | New MCP Tool |
|-------------|-------------|
| `arcade_run_task` | `vector_run_task` |
| `arcade_status` | `vector_status` |
| `arcade_cost` | `vector_cost` |

The underlying implementation in `vector_tools.py` on <terminus-host> is already updated.

## Reflexa Hook Migration

ARCADE used shell-level Reflexa hooks (`reflexa_hooks.sh`). Vector retains this for backward compatibility while adding Python-native Engram integration via `reflexa.py`.

Old hooks still work unchanged. New Vector code calls Engram directly via psycopg2/sqlite-vec.

## Work Order Format

Old ARCADE work orders (via Axon):
```json
{"op": "dev_loop", "task": "...", "repo": "..."}
```

New Vector work orders (same format, backward compatible):
```json
{"op": "dev_loop", "description": "...", "params": {"task": "...", "repo": "..."}}
```

Axon automatically handles both formats via the action-merge fix.

## What to Keep

- All Vector source code at `/opt/lumina-fleet/vector/` — no changes needed
- `reflexa_hooks.sh` — keep for T1/T2/T3 Reflexa triggers
- `memory/conventions.md` — coding conventions (unchanged)
- The Plexus (PX) project in Plane — Vector's integrated task state

## What Was Archived

- `github.com/moosenet-io/arcade` — archived (read-only, the operator action completed)
- Old MCP tools named `arcade_*` — replaced by `vector_*` in `vector_tools.py`
- Any `ARCADE` references in IronClaw LUMINA.md — replaced with `Vector`

## Verifying Migration

```bash
# On <fleet-host> — test Vector CLI
cd /opt/lumina-fleet/vector
python3 vector.py status

# On <terminus-host> — test MCP tool
python3 -c "
import sys; sys.path.insert(0, '/opt/ai-mcp')
from vector_tools import register_vector_tools
print('vector_tools: OK')
"

# Check constellation identity
python3 -c "
import sys; sys.path.insert(0, '/opt/lumina-fleet')
from naming import display_name
print(display_name('vector'))  # Should print: Vector
"
```

## Known Issues

- Integrated mode backends (Plane state + Nexus bus) are stubbed — VEC-66/67 track full implementation
- `vector.service` runs `vector.py status` by default (on-demand invocation via Axon is primary workflow)
