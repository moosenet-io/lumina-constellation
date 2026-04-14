import datetime
import json
import os
import urllib.parse
import urllib.request
from mcp.server.fastmcp import FastMCP
from gitea_tools import register_gitea_tools
from gitea_tools_lumina import register_lumina_gitea_tools
from ansible_tools import register_ansible_tools
from infisical_tools import register_infisical_tools
from prometheus_tools import register_prometheus_tools
from litellm_tools import register_litellm_tools
from dev_tools import register_dev_tools
from portainer_tools import register_portainer_tools
from jellyseerr_tools import register_jellyseerr_tools
from network_tools import register_network_tools
from google_tools import register_google_tools
from github_tools import register_github_tools
from openhands_tools import register_openhands_tools
from vector_tools import register_vector_tools
from lumina_web_tools import register_lumina_web_tools
# news_tools migrated to plugins/news_plugin.py -- loaded via plugin_loader below
# from news_tools import register_news_tools
from routines_tools import register_routines_tools
from vigil_tools import register_vigil_tools
from sentinel_tools import register_sentinel_tools
from nexus_tools import register_nexus_tools
from axon_tools import register_axon_tools
from seer_tools import register_seer_tools
from wizard_tools import register_wizard_tools
from engram_tools import register_engram_tools
from crucible_tools import register_crucible_tools
from commute_tools import register_commute_tools
from plane_tools import register_plane_tools
from odyssey_tools import register_odyssey_tools
from meridian_tools import register_meridian_tools
from vitals_tools import register_vitals_tools
from ledger_tools import register_ledger_tools
from relay_tools import register_relay_tools
from hearth_tools import register_hearth_tools
from gateway_tools import register_gateway_tools
from cortex_tools import register_cortex_tools
from myelin_tools import register_myelin_tools
from soma_tools import register_soma_tools
from dura_tools import register_dura_tools
from skills_tools import register_skills_tools

# Multi-claw: agent identity from Terminus environment
# Each IronClaw instance sets LUMINA_AGENT_ID in its stdio.sh wrapper
_AGENT_ID = os.environ.get('LUMINA_AGENT_ID', 'lumina')

def get_agent_context() -> str:
    """Return the agent_id for the current IronClaw connection.
    Defaults to 'lumina' for the primary agent.
    Partner agents set LUMINA_AGENT_ID=lumiere (or chosen name) in stdio.sh."""
    return _AGENT_ID

# Load env file if present
_env_path = "/opt/ai-mcp/.env"
if os.path.exists(_env_path):
    with open(_env_path) as _f:
        for _line in _f:
            _line = _line.strip()
            if _line and not _line.startswith("#") and "=" in _line:
                _k, _v = _line.split("=", 1)
                os.environ.setdefault(_k.strip(), _v.strip())

mcp = FastMCP("ai-mcp")
register_gitea_tools(mcp)
register_lumina_gitea_tools(mcp)
register_ansible_tools(mcp)
register_infisical_tools(mcp)
register_prometheus_tools(mcp)
register_litellm_tools(mcp)
register_dev_tools(mcp)
register_portainer_tools(mcp)
register_jellyseerr_tools(mcp)
register_network_tools(mcp)
register_google_tools(mcp)
register_github_tools(mcp)
register_openhands_tools(mcp)
register_vector_tools(mcp)
register_lumina_web_tools(mcp)
# register_news_tools(mcp)  # migrated to plugins/news_plugin.py
register_routines_tools(mcp)
register_vigil_tools(mcp)
register_sentinel_tools(mcp)
register_nexus_tools(mcp)
register_axon_tools(mcp)
register_seer_tools(mcp)
register_wizard_tools(mcp)
register_engram_tools(mcp)
register_crucible_tools(mcp)
register_commute_tools(mcp)
register_plane_tools(mcp)
register_odyssey_tools(mcp)
register_meridian_tools(mcp)
register_vitals_tools(mcp)
register_ledger_tools(mcp)
register_relay_tools(mcp)
register_hearth_tools(mcp)
register_gateway_tools(mcp)
register_cortex_tools(mcp)
register_myelin_tools(mcp)
register_soma_tools(mcp)
register_dura_tools(mcp)
register_skills_tools(mcp)

# Plugin architecture -- auto-discover tools from plugins/ directory
from plugin_loader import discover as _discover_plugins
_plugin_results = _discover_plugins(mcp)

@mcp.tool()
def health() -> dict:
    return {"ok": True}

@mcp.tool()
def echo(text: str) -> str:
    return text

@mcp.tool()
def utc_now() -> str:
    return datetime.datetime.utcnow().replace(microsecond=0).isoformat() + "Z"

@mcp.tool()
def searxng_search(
    q: str,
    categories: str = "general",
    language: str = "en-US",
) -> dict:
    """Query MooseNet SearXNG via NPM and return JSON results."""
    base = "https://search.moosenet.online/search"
    params = {"q": q, "format": "json", "categories": categories, "language": language}
    url = base + "?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(
        url,
        headers={"Accept": "application/json", "User-Agent": "ai-mcp/1.0 (+moosenet)"},
        method="GET",
    )
    with urllib.request.urlopen(req, timeout=15) as r:
        body = r.read().decode("utf-8", errors="replace")
    return json.loads(body)

if __name__ == "__main__":
    import sys
    if "--stdio" in sys.argv:
        mcp.run(transport="stdio")
    else:
        import uvicorn
        uvicorn.run(mcp.streamable_http_app(), host="0.0.0.0", port=8000)
