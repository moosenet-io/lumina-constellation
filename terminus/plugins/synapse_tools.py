"""
synapse_tools.py — Terminus MCP plugin for Synapse spontaneous conversation.

Tools:
  synapse_status   — show current config + last-sent time
  synapse_trigger  — run a manual scan now (fires synapse_scan.py --dry-run or live)
  synapse_mute     — suppress Synapse for N hours

Plugin format: exports register_plugin(mcp).
Runs on CT214 (Terminus). Synapse process lives on CT310 — commands sent via SSH.
"""

import json
import os
import subprocess
import time
from pathlib import Path
from typing import Optional


# CT310 SSH target
CT310_SSH = os.environ.get("CT310_SSH_HOST", "root@192.168.0.120")
SYNAPSE_SCAN = "/opt/lumina-fleet/synapse/synapse_scan.py"
SYNAPSE_LOG  = "/opt/lumina-fleet/synapse/gate_log.json"
PULSE_MARKERS = "/opt/lumina-fleet/pulse/markers.json"
CONSTELLATION_YAML = "/opt/lumina-fleet/constellation.yaml"

# Refractor keyword category for routing
REFRACTOR_KEYWORDS = [
    "synapse", "spontaneous", "proactive message", "mute synapse",
    "trigger synapse", "synapse status", "quiet synapse",
]


def _ssh_run(cmd: str, timeout: int = 15) -> tuple[int, str, str]:
    """Run command on CT310 via SSH. Returns (returncode, stdout, stderr)."""
    r = subprocess.run(
        ["ssh", CT310_SSH, cmd],
        capture_output=True, text=True, timeout=timeout
    )
    return r.returncode, r.stdout.strip(), r.stderr.strip()


def register_plugin(mcp):

    @mcp.tool()
    def synapse_status() -> str:
        """
        Show Synapse current config (enabled, strength, quiet hours) and when the
        last message was sent. Zero cost — reads config + log files.
        """
        try:
            # Read config from CT310
            rc, out, err = _ssh_run(
                f"python3 -c \""
                f"import yaml,json; "
                f"cfg=yaml.safe_load(open('{CONSTELLATION_YAML}')) or {{}}; "
                f"s=cfg.get('synapse',{{}}); "
                f"print(json.dumps(s))"
                f"\""
            )
            config = json.loads(out) if rc == 0 and out else {}
        except Exception as e:
            config = {"error": str(e)[:100]}

        try:
            # Read last-sent from gate_log
            rc2, out2, _ = _ssh_run(
                f"python3 -c \""
                f"import json; "
                f"log=json.load(open('{SYNAPSE_LOG}')); "
                f"last=sorted(log, key=lambda e:e.get('ts',0))[-1] if log else None; "
                f"print(json.dumps(last))"
                f"\""
            )
            last_entry = json.loads(out2) if rc2 == 0 and out2 and out2 != "null" else None
        except Exception:
            last_entry = None

        enabled = config.get("enabled", False)
        strength = config.get("strength", "moderate")
        quiet = config.get("quiet_hours", {})
        max_day = config.get("max_messages_per_day", 3)

        lines = [
            f"Synapse: {'ENABLED' if enabled else 'DISABLED'}",
            f"Strength: {strength} | Max/day: {max_day}",
            f"Quiet hours: {quiet.get('start','22:00')} – {quiet.get('end','08:00')}",
        ]
        if last_entry:
            import datetime
            ts = last_entry.get("ts", 0)
            when = datetime.datetime.fromtimestamp(ts).strftime("%a %b %d %H:%M")
            lines.append(f"Last sent: {when} | type={last_entry.get('type','')} score={last_entry.get('score',0):.2f}")
        else:
            lines.append("Last sent: never")

        if config.get("topic_blocklist"):
            lines.append(f"Blocked: {', '.join(config['topic_blocklist'])}")

        return "\n".join(lines)


    @mcp.tool()
    def synapse_trigger(dry_run: bool = True) -> str:
        """
        Run a Synapse scan manually right now.
        dry_run=True (default): shows what would be sent without sending.
        dry_run=False: actually sends the message if a candidate passes the gate.
        Returns scan output.
        """
        cmd = f"python3 {SYNAPSE_SCAN} {'--dry-run' if dry_run else ''} 2>&1"
        try:
            rc, out, err = _ssh_run(cmd, timeout=30)
            output = out or err or "(no output)"
            mode = "DRY RUN" if dry_run else "LIVE"
            return f"[synapse_trigger {mode}]\n{output}"
        except subprocess.TimeoutExpired:
            return "[synapse_trigger] Timed out after 30s"
        except Exception as e:
            return f"[synapse_trigger] Error: {e}"


    @mcp.tool()
    def synapse_mute(hours: int = 4) -> str:
        """
        Mute Synapse for the next N hours (default 4). Does this by writing a
        Pulse marker 'synapse_muted_until' with a future timestamp. The gate
        checks this marker before sending.
        Hours must be between 1 and 72.
        """
        hours = max(1, min(72, int(hours)))
        until_ts = time.time() + hours * 3600
        cmd = (
            f"python3 -c \""
            f"import json, time; "
            f"from pathlib import Path; "
            f"p=Path('{PULSE_MARKERS}'); "
            f"m=json.loads(p.read_text()) if p.exists() else {{}}; "
            f"m['synapse_muted_until']={until_ts}; "
            f"p.write_text(json.dumps(m))"
            f"\""
        )
        try:
            rc, out, err = _ssh_run(cmd, timeout=10)
            if rc == 0:
                import datetime
                until_str = datetime.datetime.fromtimestamp(until_ts).strftime("%H:%M")
                return f"Synapse muted for {hours}h (until ~{until_str}). Use synapse_trigger(dry_run=False) to override."
            return f"[synapse_mute] Failed: {err or 'unknown error'}"
        except Exception as e:
            return f"[synapse_mute] Error: {e}"
