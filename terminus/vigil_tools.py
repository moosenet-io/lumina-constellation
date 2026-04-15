import subprocess
import json

# ============================================================
# Vigil Tools (Vigil / formerly Agent Briefly)
# MCP tools that trigger briefing generation on fleet-host and
# check status. IronClaw routines call these to generate
# briefings on demand.
# Runs on terminus-host, SSHes to fleet-host to execute briefing.py.
# ============================================================

VIGIL_HOST = "root@YOUR_FLEET_SERVER_IP"
VIGIL_SCRIPT = "/usr/bin/python3 /opt/lumina-fleet/vigil/briefing.py"


def _ssh_exec(cmd, timeout=120):
    """Execute a command on fleet-host via SSH."""
    full_cmd = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {VIGIL_HOST} '{cmd}'"
    try:
        result = subprocess.run(
            full_cmd, shell=True, capture_output=True, text=True, timeout=timeout
        )
        return {"stdout": result.stdout.strip(), "stderr": result.stderr.strip(), "rc": result.returncode}
    except subprocess.TimeoutExpired:
        return {"error": "Briefing generation timed out (120s)", "rc": -1}
    except Exception as e:
        return {"error": str(e), "rc": -1}


def register_vigil_tools(mcp):

    @mcp.tool()
    def vigil_generate(briefing_type: str = "morning") -> dict:
        """Generate a briefing by triggering Vigil on fleet-host.
        briefing_type: 'morning' or 'afternoon'.
        Gathers live data (news, weather, commute, crypto, sports),
        formats with Haiku, and writes to Gitea lumina-vigil repo.
        Returns the Gitea path to the finished briefing.
        Takes ~30-60 seconds to complete."""

        if briefing_type not in ("morning", "afternoon"):
            return {"error": f"Invalid briefing type: {briefing_type}. Use 'morning' or 'afternoon'."}

        result = _ssh_exec(f"{VIGIL_SCRIPT} {briefing_type}")

        if result.get("error"):
            return {
                "status": "failed",
                "briefing_type": briefing_type,
                "error": result["error"],
            }

        if result["rc"] != 0:
            return {
                "status": "failed",
                "briefing_type": briefing_type,
                "error": result.get("stderr", "Unknown error"),
                "output": result.get("stdout", ""),
            }

        return {
            "status": "ready",
            "briefing_type": briefing_type,
            "latest_path": f"briefings/latest-{briefing_type}.md",
            "repo": "moosenet/lumina-vigil",
            "message": f"Briefing is ready. Read it from Gitea: moosenet/lumina-vigil/briefings/latest-{briefing_type}.md",
        }

    @mcp.tool()
    def vigil_status(briefing_type: str = "morning") -> dict:
        """Check if the latest briefing is available on Gitea.
        Returns the file path and last modified time if found.
        Use this for light polling instead of regenerating."""

        if briefing_type not in ("morning", "afternoon"):
            return {"error": f"Invalid briefing type: {briefing_type}. Use 'morning' or 'afternoon'."}

        # Check if the file exists on Gitea by reading its metadata via fleet-host
        check_cmd = (
            f"source /opt/briefing-agent/.infisical-auth && "
            f"TOKEN=$(curl -s -X POST $INFISICAL_URL/api/v1/auth/universal-auth/login "
            f"-H 'Content-Type: application/json' "
            f"-d '{{\"clientId\":\"'$INFISICAL_CLIENT_ID'\",\"clientSecret\":\"'$INFISICAL_CLIENT_SECRET'\"}}' "
            f"| python3 -c \"import sys,json; print(json.load(sys.stdin)['accessToken'])\") && "
            f"GITEA_TOKEN=$(curl -s \"$INFISICAL_URL/api/v3/secrets/raw/GITEA_TOKEN"
            f"?workspaceId=$SERVICES_PROJECT_ID&environment=prod&secretPath=/\" "
            f"-H \"Authorization: Bearer $TOKEN\" "
            f"| python3 -c \"import sys,json; print(json.load(sys.stdin)['secret']['secretValue'])\") && "
            f"curl -s -H \"Authorization: token $GITEA_TOKEN\" "
            f"'http://YOUR_GITEA_IP:3000/api/v1/repos/moosenet/lumina-vigil"
            f"/contents/briefings/latest-{briefing_type}.md?ref=main' "
            f"| python3 -c \"import sys,json; d=json.load(sys.stdin); "
            f"print(json.dumps({{'exists': True, 'size': d.get('size',0), 'sha': d.get('sha','')[:8]}}))\" "
            f"2>/dev/null || echo '{{\"exists\": false}}'"
        )

        result = _ssh_exec(check_cmd, timeout=30)

        if result.get("error") or result["rc"] != 0:
            return {
                "status": "unknown",
                "briefing_type": briefing_type,
                "error": result.get("error", result.get("stderr", "")),
            }

        try:
            file_info = json.loads(result["stdout"])
            if file_info.get("exists"):
                return {
                    "status": "ready",
                    "briefing_type": briefing_type,
                    "latest_path": f"briefings/latest-{briefing_type}.md",
                    "repo": "moosenet/lumina-vigil",
                    "file_info": file_info,
                }
            else:
                return {
                    "status": "not_found",
                    "briefing_type": briefing_type,
                    "message": "No briefing found. Run vigil_generate first.",
                }
        except json.JSONDecodeError:
            return {
                "status": "unknown",
                "briefing_type": briefing_type,
                "raw": result["stdout"],
            }
