import subprocess
import json

# ============================================================
# Sentinel Tools (Sentinel / formerly Agent Ops)
# MCP tools that trigger operational checks and logging on fleet-host.
# IronClaw routines call these instead of running checks via LLM.
# Runs on terminus-host, SSHes to fleet-host to execute ops.py.
# After health checks, triggers status_generator.py to update
# the live status page at http://YOUR_FLEET_SERVER_IP/status/
# ============================================================

SENTINEL_HOST = "root@YOUR_FLEET_SERVER_IP"
SENTINEL_SCRIPT = "/usr/bin/python3 /opt/lumina-fleet/sentinel/ops.py"
STATUS_GENERATOR = "/opt/lumina-fleet/sentinel/status_generator.py"
STATUS_PAGE_URL = "http://YOUR_FLEET_SERVER_IP/status/"

# Operations that trigger a status page refresh after completion
STATUS_TRIGGERING_OPS = ["system-snapshot", "self-health", "plex-health"]

VALID_OPS = [
    "plex-health", "self-health", "vm901-watchdog", "gitea-health",
    "system-snapshot", "commute-tracker", "daily-log", "reflection",
    "tool-usage-log", "memory-curation",
]


def _ssh_exec(cmd, timeout=120):
    """Execute a command on fleet-host via SSH."""
    full_cmd = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {SENTINEL_HOST} '{cmd}'"
    try:
        result = subprocess.run(
            full_cmd, shell=True, capture_output=True, text=True, timeout=timeout
        )
        return {"stdout": result.stdout.strip(), "stderr": result.stderr.strip(), "rc": result.returncode}
    except subprocess.TimeoutExpired:
        return {"error": "Operation timed out (120s)", "rc": -1}
    except Exception as e:
        return {"error": str(e), "rc": -1}


def _trigger_status_page():
    """Trigger status page regeneration on fleet-host in the background."""
    cmd = (
        "source /opt/lumina-fleet/axon/.env && "
        "export INBOX_DB_PASS PLANE_TOKEN_LUMINA && "
        f"/usr/bin/python3 {STATUS_GENERATOR} > /tmp/status-gen.log 2>&1 &"
    )
    _ssh_exec(cmd, timeout=10)


def register_sentinel_tools(mcp):

    @mcp.tool()
    def sentinel_run(operation: str, args: str = "") -> dict:
        """Run an operational check or logging task via Sentinel on fleet-host.
        Operations: plex-health, self-health, vm901-watchdog, gitea-health,
        system-snapshot, commute-tracker, daily-log, reflection,
        tool-usage-log, memory-curation.
        For commute-tracker, pass args='morning' or args='afternoon'.
        Results are written to Gitea moosenet/lumina-sentinel repo.
        After health checks (system-snapshot, self-health, plex-health),
        the live status page is automatically refreshed.
        Returns status and file paths."""

        if operation not in VALID_OPS:
            return {
                "error": f"Unknown operation: {operation}",
                "valid_operations": VALID_OPS,
            }

        cmd = f"{SENTINEL_SCRIPT} {operation}"
        if args:
            cmd += f" {args}"

        result = _ssh_exec(cmd)

        if result.get("error"):
            return {
                "status": "failed",
                "operation": operation,
                "error": result["error"],
            }

        if result["rc"] != 0:
            return {
                "status": "failed",
                "operation": operation,
                "error": result.get("stderr", "Unknown error"),
                "output": result.get("stdout", ""),
            }

        response = {
            "status": "complete",
            "operation": operation,
            "output": result.get("stdout", ""),
            "latest_path": f"checks/latest-{operation}.md" if operation in [
                "plex-health", "self-health", "vm901-watchdog", "gitea-health",
                "system-snapshot", "commute-tracker"
            ] else f"logs/latest-{operation}.md",
            "repo": "moosenet/lumina-sentinel",
        }

        # Trigger status page refresh for health-check operations
        if operation in STATUS_TRIGGERING_OPS:
            _trigger_status_page()
            response["status_page"] = STATUS_PAGE_URL
            response["status_page_refreshed"] = True

        return response

    @mcp.tool()
    def sentinel_status(operation: str = "") -> dict:
        """Check the status of operational checks. If operation is specified,
        returns the latest result for that check. If empty, returns a summary
        of all available latest checks. Also returns the live status page URL."""

        if operation and operation not in VALID_OPS:
            return {"error": f"Unknown operation: {operation}", "valid_operations": VALID_OPS}

        if operation:
            category = "checks" if operation in [
                "plex-health", "self-health", "vm901-watchdog", "gitea-health",
                "system-snapshot", "commute-tracker"
            ] else "logs"
            cmd = (f"source /opt/briefing-agent/.infisical-auth && "
                   f"TOKEN=$(curl -s -X POST $INFISICAL_URL/api/v1/auth/universal-auth/login "
                   f"-H 'Content-Type: application/json' "
                   f"-d '{{\"clientId\":\"'$INFISICAL_CLIENT_ID'\",\"clientSecret\":\"'$INFISICAL_CLIENT_SECRET'\"}}' "
                   f"| python3 -c \"import sys,json; print(json.load(sys.stdin)['accessToken'])\") && "
                   f"GITEA_TOKEN=$(curl -s \"$INFISICAL_URL/api/v3/secrets/raw/GITEA_TOKEN"
                   f"?workspaceId=$SERVICES_PROJECT_ID&environment=prod&secretPath=/\" "
                   f"-H \"Authorization: Bearer $TOKEN\" "
                   f"| python3 -c \"import sys,json; print(json.load(sys.stdin)['secret']['secretValue'])\") && "
                   f"curl -s -H \"Authorization: token $GITEA_TOKEN\" "
                   f"'http://YOUR_GITEA_IP:3000/api/v1/repos/moosenet/lumina-sentinel"
                   f"/contents/{category}/latest-{operation}.md?ref=main' "
                   f"| python3 -c \"import sys,json,base64; d=json.load(sys.stdin); "
                   f"print(base64.b64decode(d['content']).decode())\" 2>/dev/null || echo 'No data found'")
            result = _ssh_exec(cmd, timeout=30)
            return {
                "operation": operation,
                "content": result.get("stdout", "No data"),
                "status_page": STATUS_PAGE_URL,
            }

        return {
            "message": "Specify an operation to check status",
            "valid_operations": VALID_OPS,
            "status_page": STATUS_PAGE_URL,
        }

    @mcp.tool()
    def sentinel_refresh_status() -> dict:
        """Force a refresh of the MooseNet live status page at http://YOUR_FLEET_SERVER_IP/status/
        Runs all health checks and regenerates the HTML dashboard.
        Returns the status page URL and a summary of service states."""

        cmd = (
            "source /opt/lumina-fleet/axon/.env && "
            "export INBOX_DB_PASS PLANE_TOKEN_LUMINA && "
            f"/usr/bin/python3 {STATUS_GENERATOR} 2>&1"
        )
        result = _ssh_exec(cmd, timeout=60)

        return {
            "status": "refreshed",
            "status_page": STATUS_PAGE_URL,
            "output": result.get("stdout", ""),
            "error": result.get("stderr", "") or result.get("error", ""),
        }
