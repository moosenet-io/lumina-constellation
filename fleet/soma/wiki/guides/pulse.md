# Pulse: Temporal Awareness

Pulse is Lumina's zero-inference time system. It provides current date, time, period-of-day, timezone, and named markers to any fleet module — without calling an LLM and without external dependencies.

**Location**: `fleet/shared/pulse.py`

---

## Why This Exists

LLMs have no internal clock. Without explicit temporal context, a model might reference yesterday's date, use "today" ambiguously, or fail to calculate elapsed time correctly. Pulse injects compact, accurate temporal context at the token level.

The design constraint: **zero inference cost**. Pulse is pure Python, stdlib only (plus optional `pytz`). It never makes a network call. It never calls an LLM. It should never fail or add latency.

---

## Quick Start

```python
from pulse import now, short, context, mark, since, timer_start, timer_elapsed

# Current date/time
pulse.now()          # datetime object, operator timezone
pulse.date()         # "Mon Apr 14 2026"
pulse.time()         # "10:45 PM"
pulse.period()       # "morning" / "afternoon" / "evening" / "night"
pulse.greeting()     # "Good morning"

# Compact string for LLM injection (~15 tokens)
pulse.short()        # "[Mon Apr 14 10:45PM PDT morning]"
pulse.short("last_briefing")  # "[Mon Apr 14 10:45PM PDT morning | last: 3h ago]"

# Full context string (~45 tokens)
pulse.context()      # "Date: Mon Apr 14 2026 | Time: 10:45 PM PDT | Period: morning | Uptime: 3d 4h"
```

---

## Markers

Markers are named timestamps stored in `pulse/markers.json`. They persist across restarts.

```python
# Set a marker
pulse.mark("briefing_sent")           # Records current timestamp

# Read elapsed time
pulse.since("briefing_sent")          # "3h ago"
pulse.since_seconds("briefing_sent")  # 10800.0

# Auto-initialized marker
pulse.since("system_boot")            # e.g. "2d 4h ago"
```

### Common Markers Used in Fleet

| Marker | Set By | Used By |
|--------|--------|---------|
| `system_boot` | Auto-init on import | Context strings, uptime |
| `last_briefing` | Vigil (briefing.py) | Morning briefing, Pulse short() |
| `last_nexus_check` | Axon | Idle detection |
| `vector_task_start_{id}` | Vector | Timer display in Soma |
| `sentinel_last_scan` | Sentinel | Health check timestamps |

---

## Timers

Timers are named markers with human-readable elapsed output. Used for long-running processes like Vector loops.

```python
# Start a timer
pulse.timer_start("vector_task_abc123")

# Read elapsed
pulse.timer_elapsed("vector_task_abc123")         # "4m 32s"
pulse.timer_elapsed_seconds("vector_task_abc123") # 272.0
```

The Vector mission control page in Soma uses `timer_elapsed()` for the loop duration display in each active task card.

---

## LLM Context Injection

### Minimal injection (recommended)
Use `short()` when you need the model to know the time but don't want to spend tokens on it:

```python
prompt = f"""
{pulse.short("last_briefing")}

Summarize today's calendar events...
"""
```

Output: `[Mon Apr 14 10:45PM PDT morning | last: 3h ago]`
~15 tokens. Enough for temporal grounding.

### Full context injection
Use `context()` only when elapsed time, uptime, or period are important to the task:

```python
prompt = f"""
System context: {pulse.context()}

Evaluate whether this alert requires immediate action...
"""
```

Output: `Date: Mon Apr 14 2026 | Time: 10:45 PM PDT | Period: evening | Uptime: 3d 4h`
~45 tokens.

### Never inject Pulse into every prompt
Temporal context has a cost. Most tool calls don't need the date. Add `short()` when:
- The response involves scheduling, timing, or elapsed time
- The model might anchor to a wrong date (long conversations)
- You're generating a briefing, report, or summary

Don't add it to:
- Tool parameter construction
- JSON parsing calls
- Simple API forwarding

---

## Timezone Configuration

Pulse reads timezone from `constellation.yaml`:

```yaml
timezone: "America/Los_Angeles"  # IANA timezone name
```

Falls back to `America/Los_Angeles` if not set. If `pytz` is not installed, falls back to UTC (all timestamps will be UTC — install pytz for correct local time).

```bash
pip install pytz  # required for correct timezone on fleet-host
```

---

## Period-of-Day

Pulse defines 4 periods:

| Period | Hours | Used For |
|--------|-------|---------|
| morning | 05:00–11:59 | Briefings, proactive tasks |
| afternoon | 12:00–16:59 | Operational tasks |
| evening | 17:00–20:59 | Low-priority, wrap-up |
| night | 21:00–04:59 | Silent mode, Synapse quiet hours |

Modules check `pulse.period()` to gate behavior:
- Vigil only generates morning briefings during `morning`
- Synapse suppresses non-urgent messages during `evening` and `night`
- Vector doesn't start new loops at `night` unless explicitly forced

---

## Integration Points

| Module | Pulse Usage |
|--------|------------|
| `vigil/briefing.py` | `short()` injected into briefing prompt; `mark("last_briefing")` on send |
| `vector/executor.py` | `timer_start/elapsed` for loop duration display |
| `sentinel/ops.py` | `since()` for last-scan timestamps in status grid |
| `axon/axon.py` | `since("last_nexus_check")` for idle detection |
| `soma/main.py` | `short()` in Soma API responses; `since()` for uptime display |
| `obsidian_circle/engine.py` | (not used — Circle uses wall clock directly) |

---

## Pulse MCP Tools (Terminus)

Six tools are registered in Terminus for Lumina to call:

| Tool | Returns |
|------|---------|
| `pulse_now` | Current datetime string |
| `pulse_short` | Compact string (with optional last_marker) |
| `pulse_context` | Full context string |
| `pulse_mark` | Sets a named marker |
| `pulse_since` | Elapsed since a marker |
| `pulse_timer_elapsed` | Elapsed for a named timer |

Lumina uses `pulse_short()` in the Refractor "core" category (always visible) so it always has temporal grounding without spending tokens.

---

## Files

```
fleet/shared/pulse.py              # Core module
fleet/pulse/markers.json           # Persistent marker store (auto-created)
terminus/pulse_tools.py            # MCP tool registration
```

---

## Token Cost

| Function | Approx Output Tokens |
|----------|---------------------|
| `short()` | ~15 |
| `context()` | ~45 |
| `date()` | ~5 |
| `period()` | ~2 |

Zero inference cost (Python only). Output tokens are consumed by the LLM reading the context — keep injections minimal.
