# /opt/ai-mcp/plugins/example_plugin.py
"""
Example plugin demonstrating the Terminus plugin architecture.
Drop any .py file in /opt/ai-mcp/plugins/ to add new MCP tools without
modifying server.py.
"""
import os
from datetime import datetime

# Option A: export register_plugin(mcp) function
def register_plugin(mcp):

    @mcp.tool()
    def constellation_version() -> dict:
        """Return Lumina Constellation version info and build metadata.
        Use this to verify the MCP server is running and check deployment info."""
        return {
            'constellation': 'Lumina Constellation',
            'version': '0.12.0',
            'session': 12,
            'mcp_hub': 'CT214 (Terminus)',
            'agent_fleet': 'CT310',
            'orchestrator': 'CT305 (IronClaw v0.24.0)',
            'plugin_architecture': True,
            'skills_standard': 'agentskills.io',
            'timestamp': datetime.utcnow().isoformat() + 'Z',
        }
