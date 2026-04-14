"""vector_onboard — Returns Vector operating context for any agent connecting to it."""
import os


def register_plugin(mcp):

    @mcp.tool()
    def vector_onboard() -> dict:
        """Get Vector operating manual. Call this before delegating work to Vector.
        Returns: guardrails, active projects, conventions, available models, how to submit tasks.

        Any agent (Lumina, Seer, etc.) should call this before their first Vector interaction
        in a session to understand current state and operating rules."""
        import json
        import subprocess
        from pathlib import Path

        fleet_dir = Path('/opt/lumina-fleet')

        # System guardrails
        guardrails = []
        guardrails_file = fleet_dir / 'vector' / 'system-guardrails.md'
        if guardrails_file.exists():
            guardrails = [l.strip() for l in guardrails_file.read_text().splitlines()
                          if l.strip() and not l.startswith('#')][:10]

        # Active projects
        projects_dir = fleet_dir / 'vector' / 'vector-projects'
        active_projects = []
        if projects_dir.exists():
            for p in projects_dir.iterdir():
                if p.is_dir():
                    active_projects.append(p.name)

        # Operating conventions from Engram (best-effort)
        conventions = []
        try:
            result = subprocess.run(
                ['ssh', '-o', 'ConnectTimeout=3', 'root@YOUR_FLEET_SERVER_IP',
                 'python3 /opt/lumina-fleet/engram/engram.py query --text "vector conventions" --top-k 3'],
                capture_output=True, text=True, timeout=10
            )
            if result.returncode == 0:
                conventions = json.loads(result.stdout) if result.stdout.strip() else []
        except Exception:
            pass

        return {
            'agent': 'vector',
            'version': '1.0',
            'status': 'active',
            'system_guardrails': guardrails or [
                'Never merge own PRs',
                'Write tests before committing',
                'Cost gate max $2/task',
            ],
            'active_projects': active_projects,
            'conventions': conventions[:3],
            'how_to_submit': {
                'via_nexus': (
                    "nexus_send(from_agent='lumina', to_agent='vector', message_type='work_order', "
                    "payload=json.dumps({'op':'maintenance','task':'<description>','repo':'<path>'}))"
                ),
                'via_mcp': "vector_submit(task='<description>', repo='<path>', cost_budget=2.0)",
            },
            'cost_limits': {'max_per_task': 2.0, 'max_per_day': 10.0},
            'calx_active': True,
            'skill_aware': True,
        }
