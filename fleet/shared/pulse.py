"""
pulse.py — Temporal awareness for Lumina Constellation.

Zero inference cost. Pure Python. No external dependencies beyond stdlib + pytz.

Usage:
    from pulse import now, short, context, mark, since, since_seconds, greeting
    from pulse import timer_start, timer_elapsed

All timestamps are in the operator's timezone (from constellation.yaml or default).
"""

import json
import os
import time as _time
from datetime import datetime, timedelta
from pathlib import Path

# Optional pytz — falls back to UTC offset if unavailable
try:
    import pytz
    _HAS_PYTZ = True
except ImportError:
    _HAS_PYTZ = False

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

_DEFAULT_TZ = "America/Los_Angeles"
_MARKERS_PATH = Path(os.environ.get("PULSE_MARKERS_PATH", "/opt/lumina-fleet/pulse/markers.json"))
_CONSTELLATION_PATH = Path(os.environ.get("CONSTELLATION_PATH", "/opt/lumina-fleet/constellation.yaml"))

_PERIODS = [
    (5,  "night"),
    (12, "morning"),
    (17, "afternoon"),
    (21, "evening"),
    (24, "night"),
]

_GREETINGS = {
    "morning":   "Good morning",
    "afternoon": "Good afternoon",
    "evening":   "Good evening",
    "night":     "Good evening",
}


def _get_timezone():
    """Read timezone from constellation.yaml, fall back to default."""
    tz_name = _DEFAULT_TZ
    if _CONSTELLATION_PATH.exists():
        try:
            with open(_CONSTELLATION_PATH) as f:
                for line in f:
                    line = line.strip()
                    if line.startswith("timezone:"):
                        tz_name = line.split(":", 1)[1].strip().strip('"\'')
                        break
        except Exception:
            pass
    return tz_name


def _now_local() -> datetime:
    """Return current datetime in operator timezone."""
    tz_name = _get_timezone()
    if _HAS_PYTZ:
        tz = pytz.timezone(tz_name)
        return datetime.now(tz)
    else:
        # Fallback: UTC only
        return datetime.utcnow()


# ---------------------------------------------------------------------------
# Core accessors
# ---------------------------------------------------------------------------

def now() -> datetime:
    """Current datetime in operator timezone."""
    return _now_local()


def date() -> str:
    """Today's date. e.g. 'Mon Apr 14 2026'"""
    return _now_local().strftime("%a %b %d %Y")


def time() -> str:
    """Current time. e.g. '10:45 PM'"""
    return _now_local().strftime("%-I:%M %p")


def period() -> str:
    """Time-of-day period: morning / afternoon / evening / night."""
    h = _now_local().hour
    for cutoff, label in _PERIODS:
        if h < cutoff:
            return label
    return "night"


def greeting() -> str:
    """Contextual greeting. e.g. 'Good morning'"""
    return _GREETINGS[period()]


def tz_abbr() -> str:
    """Timezone abbreviation. e.g. 'PDT'"""
    dt = _now_local()
    if _HAS_PYTZ:
        return dt.strftime("%Z")
    return "UTC"


# ---------------------------------------------------------------------------
# Compact string representations
# ---------------------------------------------------------------------------

def short(last_marker: str = None) -> str:
    """
    ~15 token compact string for LLM context injection.
    e.g. '[Mon Apr 14 10:45PM PDT evening | last: 2h ago]'
    """
    dt = _now_local()
    date_str = dt.strftime("%a %b %d")
    time_str = dt.strftime("%-I:%M%p")
    tz = tz_abbr()
    p = period()

    if last_marker:
        elapsed = since(last_marker)
        suffix = f" | last: {elapsed}" if elapsed else ""
    else:
        suffix = ""

    return f"[{date_str} {time_str} {tz} {p}{suffix}]"


def context() -> str:
    """
    ~45 token full context string. Opt-in — not injected by default.
    e.g. 'Date: Mon Apr 14 2026 | Time: 10:45 PM PDT | Period: evening | Uptime: 3d 4h'
    """
    parts = [
        f"Date: {date()}",
        f"Time: {time()} {tz_abbr()}",
        f"Period: {period()}",
    ]

    up = since_seconds("system_boot")
    if up is not None:
        parts.append(f"Uptime: {_fmt_elapsed(up)}")

    return " | ".join(parts)


# ---------------------------------------------------------------------------
# Markers
# ---------------------------------------------------------------------------

def _load_markers() -> dict:
    if _MARKERS_PATH.exists():
        try:
            with open(_MARKERS_PATH) as f:
                return json.load(f)
        except Exception:
            pass
    return {}


def _save_markers(markers: dict):
    _MARKERS_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(_MARKERS_PATH, "w") as f:
        json.dump(markers, f, indent=2)


def mark(name: str) -> float:
    """Set a named marker to now. Returns the Unix timestamp."""
    markers = _load_markers()
    ts = _time.time()
    markers[name] = ts
    _save_markers(markers)
    return ts


def since(name: str) -> str:
    """Human-readable elapsed time since marker. e.g. '2h ago', '3d ago'."""
    secs = since_seconds(name)
    if secs is None:
        return None
    return _fmt_elapsed(secs)


def since_seconds(name: str) -> float:
    """Seconds elapsed since marker. Returns None if marker not set."""
    markers = _load_markers()
    if name not in markers:
        return None
    return _time.time() - markers[name]


def _fmt_elapsed(secs: float) -> str:
    secs = int(secs)
    if secs < 60:
        return f"{secs}s ago"
    elif secs < 3600:
        return f"{secs // 60}m ago"
    elif secs < 86400:
        h = secs // 3600
        m = (secs % 3600) // 60
        return f"{h}h {m}m ago" if m else f"{h}h ago"
    else:
        d = secs // 86400
        h = (secs % 86400) // 3600
        return f"{d}d {h}h ago" if h else f"{d}d ago"


# ---------------------------------------------------------------------------
# Timers (for Vector loops and long tasks)
# ---------------------------------------------------------------------------

def timer_start(timer_id: str) -> float:
    """Start a named timer. Returns start timestamp."""
    return mark(f"timer_{timer_id}")


def timer_elapsed(timer_id: str) -> str:
    """Human-readable elapsed time for a timer. e.g. '4m 32s'"""
    secs = since_seconds(f"timer_{timer_id}")
    if secs is None:
        return "not started"
    secs = int(secs)
    if secs < 60:
        return f"{secs}s"
    elif secs < 3600:
        m, s = divmod(secs, 60)
        return f"{m}m {s}s"
    else:
        h, rem = divmod(secs, 3600)
        m = rem // 60
        return f"{h}h {m}m"


def timer_elapsed_seconds(timer_id: str) -> float:
    """Raw seconds elapsed for a timer."""
    return since_seconds(f"timer_{timer_id}")


# ---------------------------------------------------------------------------
# Auto-markers on import
# ---------------------------------------------------------------------------

def _auto_init():
    markers = _load_markers()
    if "system_boot" not in markers:
        # Approximate boot time from /proc/uptime if available
        try:
            with open("/proc/uptime") as f:
                uptime_secs = float(f.read().split()[0])
            boot_ts = _time.time() - uptime_secs
        except Exception:
            boot_ts = _time.time()
        markers["system_boot"] = boot_ts
        _save_markers(markers)


try:
    _auto_init()
except Exception:
    pass  # Never crash on import
