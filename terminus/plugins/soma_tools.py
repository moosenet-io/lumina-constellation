"""Soma MCP tools — admin panel integration for Lumina agents.
Gives IronClaw access to Soma admin capabilities via MCP.
"""
import os, json, urllib.request

SOMA_URL = os.environ.get('SOMA_URL', 'http://YOUR_FLEET_SERVER_IP:8082')
SOMA_KEY = os.environ.get('SOMA_SECRET_KEY', 'soma-dev-key')

def _soma(path, method='GET', data=None):
    req = urllib.request.Request(
        f'{SOMA_URL}{path}',
        data=json.dumps(data).encode() if data else None,
        headers={'X-Soma-Key': SOMA_KEY, 'Content-Type': 'application/json'},
        method=method
    )
    try:
        with urllib.request.urlopen(req, timeout=8) as r:
            return json.load(r)
    except Exception as e:
        return {'error': str(e)[:100]}

def register_plugin(mcp):

    @mcp.tool()
    def soma_status() -> dict:
        """Get Lumina Constellation system health summary from Soma admin panel.
        Returns IronClaw version, agent status, Nexus inbox count, Engram facts, inference cost."""
        return _soma('/api/system/health')

    @mcp.tool()
    def soma_skills_list() -> dict:
        """List all active and proposed agent skills from the skills directory.
        Returns active skills (ready to use) and proposed skills (awaiting approval)."""
        return _soma('/api/skills')

    @mcp.tool()
    def soma_skill_approve(skill_name: str) -> dict:
        """Approve a proposed skill, moving it from proposed/ to active/.
        skill_name: the skill directory name (e.g. 'morning-briefing-v2')"""
        return _soma(f'/api/skills/{skill_name}/approve', method='POST')

    @mcp.tool()
    def soma_modules() -> dict:
        """Get status of all Lumina modules (enabled/disabled, running/stopped)."""
        return _soma('/api/modules')
