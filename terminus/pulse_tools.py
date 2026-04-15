"""
pulse_tools.py — Pulse temporal awareness MCP tools (Terminus MCP hub).
Exposes pulse.py as MCP-callable tools for IronClaw.

Tools (6):
    pulse_now           — current date/time/period string (~15 tokens)
    pulse_since         — elapsed time since a named marker
    pulse_mark          — set a named timestamp marker
    pulse_context       — full context injection string (~45 tokens)
    pulse_timer_start   — start a named duration timer
    pulse_timer_elapsed — elapsed time for a named timer

Add to Refractor category: temporal
"""

import os
import sys

_FLEET_DIR = os.environ.get('FLEET_DIR', '/opt/lumina-fleet')
sys.path.insert(0, os.path.join(_FLEET_DIR, 'shared'))

try:
    import pulse as _pulse
    _PULSE_OK = True
except ImportError:
    _PULSE_OK = False


def register_pulse_tools(mcp):

    @mcp.tool()
    def pulse_now() -> str:
        """Return the current date, time, timezone abbreviation, and time-of-day period.
        Period is one of: morning (5am-12pm), afternoon (12pm-5pm), evening (5pm-9pm), night (9pm-5am).
        Use when you need temporal context without injecting the full ~45-token context string."""
        if not _PULSE_OK:
            from datetime import datetime, timezone
            return datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ (UTC, pulse unavailable)')
        import pulse as p
        return f"{p.date()} {p.time()} {p.tz_abbr()} ({p.period()})"

    @mcp.tool()
    def pulse_since(marker: str) -> str:
        """Return how long ago a named marker was set.
        Returns a human-readable string like '2h ago', '3d ago', '45m ago'.
        Returns 'not set' if the marker has never been created.
        Common system markers: vigil_morning_sent, vigil_afternoon_sent,
        axon_last_poll, last_nexus_message, system_boot."""
        if not _PULSE_OK:
            return 'pulse unavailable'
        import pulse as p
        result = p.since(marker)
        return result if result is not None else f"'{marker}' not set"

    @mcp.tool()
    def pulse_mark(name: str) -> str:
        """Record a named event timestamp (now). Persists across service restarts.
        Returns confirmation with the ISO timestamp.
        Examples: pulse_mark('plan_review_done'), pulse_mark('last_nexus_message')"""
        if not _PULSE_OK:
            return 'pulse unavailable'
        import pulse as p
        from datetime import datetime, timezone
        ts = p.mark(name)
        iso = datetime.fromtimestamp(ts, timezone.utc).isoformat()
        return f"Marked '{name}' at {iso}"

    @mcp.tool()
    def pulse_context() -> str:
        """Return the full temporal context string for prompt injection (~45 tokens).
        Includes: current date, time, timezone, time-of-day period, and system uptime.
        Use this for briefing prompts and anywhere the agent needs full temporal awareness.
        For casual checks, prefer pulse_now() (~15 tokens)."""
        if not _PULSE_OK:
            from datetime import datetime, timezone
            return f"Date: {datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M UTC')} (pulse module unavailable)"
        import pulse as p
        return p.context()

    @mcp.tool()
    def pulse_timer_start(timer_id: str) -> str:
        """Start a named duration timer. The timer persists in markers.json.
        Use before starting a long-running task to enable elapsed-time tracking.
        Example: pulse_timer_start('vector_loop_42') before launching a Vector loop."""
        if not _PULSE_OK:
            return 'pulse unavailable'
        import pulse as p
        p.timer_start(timer_id)
        return f"Timer '{timer_id}' started"

    @mcp.tool()
    def pulse_timer_elapsed(timer_id: str) -> str:
        """Return elapsed time for a named timer as a human-readable string.
        Returns '14m 23s', '2h 5m', etc. Returns 'not started' if timer doesn't exist.
        Use alongside pulse_timer_start() to track loop durations."""
        if not _PULSE_OK:
            return 'pulse unavailable'
        import pulse as p
        return p.timer_elapsed(timer_id)
