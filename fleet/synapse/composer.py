"""
composer.py — Synapse Stage 3: Message Composition
MooseNet · Document 26 implementation

Composes a natural 1-3 sentence message for each approved trigger.
Uses local Qwen via Ollama ($0). Falls back to template if Ollama unavailable.
Sends via Matrix (or dry-run to stdout).

Usage:
    from composer import SynapseComposer
    composer = SynapseComposer(config)
    composer.compose_and_send(candidate)
"""

import json
import os
import sys
import time
from pathlib import Path
from typing import Any

# Optional Ollama client
try:
    import urllib.request
    _HAS_URLLIB = True
except ImportError:
    _HAS_URLLIB = False

# Optional Matrix send — Vigil uses the same approach
_MATRIX_SEND_PATH = Path(os.environ.get(
    "MATRIX_SEND_SCRIPT",
    "/opt/lumina-fleet/shared/matrix_send.py"
))

OLLAMA_BASE_URL = os.environ.get("OLLAMA_BASE_URL", "http://192.168.0.225:11434")
OLLAMA_MODEL    = os.environ.get("SYNAPSE_LLM_MODEL", "qwen2.5:7b")
FALLBACK_MODEL  = os.environ.get("SYNAPSE_FALLBACK_MODEL", "openrouter/google/gemini-flash-1.5")
MATRIX_ROOM     = os.environ.get("MATRIX_ROOM_ID", "")
MATRIX_TOKEN    = os.environ.get("MATRIX_TOKEN", "")
MATRIX_SERVER   = os.environ.get("MATRIX_SERVER", "http://192.168.0.208:8008")

# ---------------------------------------------------------------------------
# Message templates (fallback when Ollama is down)
# ---------------------------------------------------------------------------

_TEMPLATES = {
    "engram_new_fact": "Something new came up in my memory worth mentioning: {content}",
    "engram_hub_node": "I've noticed a recurring theme in recent conversations: {content} — it's come up {link_count} times recently.",
    "engram_needs_review": "Quick flag: there may be a contradiction in something I know about '{key}'. Worth a look when you have a moment.",
    "pulse_stale_topic": "It's been {elapsed_days} days since we last talked about {topic}. Still on your radar?",
    "pulse_weekly_recap": "Happy Friday — want a quick recap of the week's highlights?",
    "sentinel_resolved": "Good news: the {issue} issue on {host} has been resolved.",
    "vigil_news": "Vigil spotted something you might find interesting: {title}",
    "vector_completed": "Vector wrapped up '{task}' — letting you know in case you wanted to review.",
    "nexus_pending": "{sender} left a message: '{subject}'",
}

_ATTRIBUTION = {
    "Engram":   "I noticed",
    "Pulse":    "Just a heads-up",
    "Sentinel": "Sentinel noticed",
    "Vigil":    "Vigil spotted",
    "Vector":   "Vector finished",
    "Nexus":    "Incoming from {sender}:",
}


def _fill_template(template: str, data: dict) -> str:
    """Safe format — missing keys become '?' instead of raising."""
    try:
        return template.format(**data)
    except KeyError:
        # Fill what we can
        result = template
        for k, v in data.items():
            result = result.replace("{" + k + "}", str(v))
        return result


class SynapseComposer:
    """
    Composes and dispatches Synapse messages.
    Tries Ollama first; falls back to templates.
    """

    def __init__(self, config: dict):
        self.config = config
        self.dry_run = config.get("dry_run", False)
        self.operator_name = config.get("operator_name", "Peter")

    def compose_and_send(self, candidate: dict) -> str:
        """Compose a message for the candidate and send it. Returns the message."""
        msg = self._compose(candidate)
        if not msg:
            return ""
        if self.dry_run:
            print(f"[DRY RUN] {msg}")
        else:
            self._send_matrix(msg)
        return msg

    def _compose(self, candidate: dict) -> str:
        """Try Ollama, fall back to template."""
        # Try local Ollama first
        if _HAS_URLLIB:
            msg = self._compose_ollama(candidate)
            if msg:
                return msg
        # Fall back to template
        return self._compose_template(candidate)

    def _compose_ollama(self, candidate: dict) -> str:
        """Generate message via local Qwen. Returns empty string on failure."""
        trigger_type = candidate["type"]
        source = candidate["source"]
        data = candidate.get("data", {})

        prompt = f"""You are Lumina, a personal AI assistant. Write a single natural message (1-3 sentences) to {self.operator_name} about the following.

Trigger type: {trigger_type}
Source: {source}
Data: {json.dumps(data, indent=2)[:500]}

Rules:
- Casual, brief, like a colleague mentioning something useful
- Attribution: start with "{_ATTRIBUTION.get(source, source)} —"
- Never create urgency or guilt-trip
- End with an easy dismiss: "No action needed." or "Just FYI."
- No emoji unless requested
- Max 3 sentences

Message:"""

        try:
            payload = json.dumps({
                "model": OLLAMA_MODEL,
                "prompt": prompt,
                "stream": False,
                "options": {"num_predict": 150, "temperature": 0.7},
            }).encode()
            req = urllib.request.Request(
                f"{OLLAMA_BASE_URL}/api/generate",
                data=payload,
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=15) as resp:
                result = json.loads(resp.read())
                return result.get("response", "").strip()
        except Exception:
            return ""

    def _compose_template(self, candidate: dict) -> str:
        """Build message from hardcoded template. Always works."""
        trigger_type = candidate["type"]
        source = candidate["source"]
        data = candidate.get("data", {})

        template = _TEMPLATES.get(trigger_type, "Update from {source}: {summary}")
        prefix = _ATTRIBUTION.get(source, source) + " — "

        if trigger_type == "nexus_pending":
            prefix = _fill_template(_ATTRIBUTION.get(source, "{sender}:"), data) + " "

        body = _fill_template(template, data)
        return prefix + body

    def _send_matrix(self, message: str):
        """Send message via Matrix. Tries matrix_send.py script first."""
        if not MATRIX_ROOM or not MATRIX_TOKEN:
            print(f"[Synapse] No Matrix config — message: {message}", file=sys.stderr)
            return

        if _MATRIX_SEND_PATH.exists():
            self._send_via_script(message)
        else:
            self._send_via_api(message)

    def _send_via_script(self, message: str):
        """Use shared matrix_send.py helper if available."""
        import subprocess
        try:
            subprocess.run(
                ["python3", str(_MATRIX_SEND_PATH), MATRIX_ROOM, message],
                timeout=10,
                check=True,
                env={**os.environ, "MATRIX_TOKEN": MATRIX_TOKEN, "MATRIX_SERVER": MATRIX_SERVER},
            )
        except Exception as e:
            print(f"[Synapse] matrix_send.py failed: {e}", file=sys.stderr)
            self._send_via_api(message)

    def _send_via_api(self, message: str):
        """Direct Matrix API call."""
        if not _HAS_URLLIB:
            return
        try:
            txn_id = f"synapse_{int(time.time())}"
            url = f"{MATRIX_SERVER}/_matrix/client/v3/rooms/{MATRIX_ROOM}/send/m.room.message/{txn_id}"
            payload = json.dumps({
                "msgtype": "m.text",
                "body": message,
            }).encode()
            req = urllib.request.Request(
                url,
                data=payload,
                headers={
                    "Content-Type": "application/json",
                    "Authorization": f"Bearer {MATRIX_TOKEN}",
                },
                method="PUT",
            )
            with urllib.request.urlopen(req, timeout=10) as resp:
                pass  # success
        except Exception as e:
            print(f"[Synapse] Matrix API error: {e}", file=sys.stderr)
