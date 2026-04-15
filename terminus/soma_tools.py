"""
soma_tools.py — Soma Admin Panel MCP tools for Terminus (terminus-host)
7 tools for managing the Lumina Constellation admin panel.
Soma runs on fleet-host:8082. Auth via X-Soma-Key header.
"""

import os
import json
import urllib.request
import urllib.error
from typing import Optional

SOMA_BASE = os.environ.get("SOMA_URL", "http://YOUR_FLEET_SERVER_IP:8082")


def _soma_key() -> str:
    return os.environ.get("SOMA_SECRET_KEY", "soma-dev-key")


def _soma_get(path: str) -> dict:
    url = f"{SOMA_BASE}{path}"
    req = urllib.request.Request(url, headers={"X-Soma-Key": _soma_key()})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}"}
    except Exception as e:
        return {"error": str(e)[:200]}


def _soma_post(path: str, payload: dict = None) -> dict:
    url = f"{SOMA_BASE}{path}"
    data = json.dumps(payload or {}).encode()
    req = urllib.request.Request(
        url, data=data,
        headers={"X-Soma-Key": _soma_key(), "Content-Type": "application/json"},
        method="POST"
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}"}
    except Exception as e:
        return {"error": str(e)[:200]}


def _soma_put(path: str, payload: dict) -> dict:
    url = f"{SOMA_BASE}{path}"
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        url, data=data,
        headers={"X-Soma-Key": _soma_key(), "Content-Type": "application/json"},
        method="PUT"
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}"}
    except Exception as e:
        return {"error": str(e)[:200]}


def register_soma_tools(mcp):

    @mcp.tool()
    def soma_status() -> dict:
        """Check if Soma admin API is up. Returns version and status."""
        try:
            url = f"{SOMA_BASE}/health"
            req = urllib.request.Request(url)
            with urllib.request.urlopen(req, timeout=5) as r:
                data = json.loads(r.read().decode())
            data["url"] = SOMA_BASE
            return data
        except Exception as e:
            return {"status": "unreachable", "error": str(e)[:150], "url": SOMA_BASE}

    @mcp.tool()
    def soma_rename_agent(agent_id: str, display_name: str) -> dict:
        """
        Rename an agent's display name in constellation.yaml via Soma.
        agent_id: internal agent key (e.g. 'vigil', 'axon', 'lumina')
        display_name: new human-readable name
        """
        return _soma_put(
            f"/api/constellation/agent/{agent_id}/display_name",
            {"name": display_name}
        )

    @mcp.tool()
    def soma_constellation_config() -> dict:
        """
        Get the current constellation.yaml config via Soma.
        Returns all agents, modules, and system metadata.
        """
        return _soma_get("/api/constellation")

    @mcp.tool()
    def soma_inference_status() -> dict:
        """
        Check LiteLLM inference layer status via Soma.
        Returns list of available models and online/error status.
        """
        return _soma_get("/api/inference/status")

    @mcp.tool()
    def soma_cost_summary() -> dict:
        """
        Get Myelin cost/token usage summary via Soma.
        Returns daily/weekly spend data if Myelin is collecting.
        """
        return _soma_get("/api/cost")

    @mcp.tool()
    def soma_backup_status() -> dict:
        """
        Get Dura backup status via Soma.
        Returns last backup run time, success/failure, and file counts.
        """
        return _soma_get("/api/backup/status")

    @mcp.tool()
    def soma_run_validation() -> dict:
        """
        Trigger a Dura smoke test run via Soma.
        Runs asynchronously — check soma_validation_status() for results.
        Returns pid of the background test process.
        """
        return _soma_post("/api/validate/smoke-test")
