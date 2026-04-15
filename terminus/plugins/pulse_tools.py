"""
pulse_tools.py — Terminus MCP plugin for Pulse temporal awareness.

Provides time/date context and marker tracking to Lumina and agents.
Zero inference cost — pure Python reads.

Plugin format: exports register_plugin(mcp).
"""

import sys
import os

# Pulse lives in /opt/lumina-fleet/shared/ on CT310, but on CT214 we import
# from the local copy deployed alongside this plugin.
_PULSE_PATHS = [
    "/opt/lumina-fleet/shared",
    "/opt/ai-mcp/plugins",
    os.path.dirname(__file__),
]
for _p in _PULSE_PATHS:
    if _p not in sys.path:
        sys.path.insert(0, _p)

import pulse


def register_plugin(mcp):

    @mcp.tool()
    def pulse_now() -> str:
        """
        Current date, time, timezone, and period of day.
        Returns compact string like '[Mon Apr 14 10:45PM PDT evening]'.
        Zero cost — no inference.
        """
        return pulse.short()

    @mcp.tool()
    def pulse_context() -> str:
        """
        Full temporal context string (~45 tokens).
        Returns: 'Date: Mon Apr 14 2026 | Time: 10:45 PM PDT | Period: evening | Uptime: 3d 4h'
        Use when you need rich temporal context injected into a prompt. Zero cost.
        """
        return pulse.context()

    @mcp.tool()
    def pulse_mark(name: str) -> str:
        """
        Set a named time marker to now. Use to record when an event happened.
        Persists to disk — survives agent restarts.
        Returns: 'Marked {name} at {timestamp}'
        """
        ts = pulse.mark(name)
        return f"Marked '{name}' at {pulse.now().strftime('%a %b %d %H:%M:%S %Z')}"

    @mcp.tool()
    def pulse_since(name: str) -> str:
        """
        How long ago a named marker was set. e.g. '2h ago', '3d ago'.
        Returns None if marker not found.
        Use for: 'when did I last check X?', 'how long has Y been running?'
        """
        result = pulse.since(name)
        if result is None:
            return f"No marker named '{name}' found."
        return f"'{name}': {result}"

    @mcp.tool()
    def pulse_timer_start(timer_id: str) -> str:
        """
        Start a named timer. Use at the beginning of a long task or Vector loop.
        Returns: 'Timer {id} started at {time}'
        """
        pulse.timer_start(timer_id)
        return f"Timer '{timer_id}' started at {pulse.now().strftime('%H:%M:%S %Z')}"

    @mcp.tool()
    def pulse_timer_elapsed(timer_id: str) -> str:
        """
        How long a named timer has been running. e.g. '4m 32s', '1h 12m'.
        Returns 'not started' if timer_start was never called.
        Use to report task duration mid-loop or at completion.
        """
        elapsed = pulse.timer_elapsed(timer_id)
        return f"Timer '{timer_id}': {elapsed}"
