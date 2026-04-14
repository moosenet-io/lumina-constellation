import subprocess
import json
import os

# ============================================================
# Wizard Tools — Mr. Wizard Deep Reasoning Agent
# MCP tools for Lumina to consult Mr. Wizard and the
# Obsidian Circle. Runs on CT214, SSHes to CT310.
# ============================================================

WIZARD_HOST = 'root@YOUR_FLEET_SERVER_IP'
WIZARD_SCRIPT = '/usr/bin/python3 /opt/lumina-fleet/wizard/wizard.py'
WIZARD_ENV = 'source /opt/lumina-fleet/axon/.env && export INBOX_DB_HOST INBOX_DB_USER INBOX_DB_PASS GITEA_TOKEN LITELLM_MASTER_KEY GITEA_URL'


def _ssh_exec(cmd, timeout=180):
    full_cmd = f'ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {WIZARD_HOST} \'{cmd}\''
    try:
        result = subprocess.run(full_cmd, shell=True, capture_output=True, text=True, timeout=timeout)
        return {'stdout': result.stdout.strip(), 'stderr': result.stderr.strip(), 'rc': result.returncode}
    except subprocess.TimeoutExpired:
        return {'error': f'Wizard consultation timed out ({timeout}s)', 'rc': -1}
    except Exception as e:
        return {'error': str(e), 'rc': -1}


def register_wizard_tools(mcp):

    @mcp.tool()
    def wizard_consult(
        query: str,
        context: str = '',
        cortex_files: str = '',
    ) -> dict:
        """Consult the Obsidian Circle for deep reasoning on complex questions.
        query: the question or decision to analyze.
        context: optional background context.
        cortex_files: optional comma-separated file paths for Cortex code analysis (e.g. 'axon/axon.py,gateway.py')
        Returns: {synthesis, member_responses, total_cost, confidence}"""

        context_arg = f'--context "{context[:500]}"' if context else ''
        files_arg = f"--cortex-files '{cortex_files}'" if cortex_files else ''

        cmd = f'{WIZARD_ENV} && {WIZARD_SCRIPT} --query "{query[:500]}" --parallel {context_arg} {files_arg} 2>&1'
        result = _ssh_exec(cmd, timeout=300)

        if result.get('error'):
            return {'status': 'failed', 'error': result['error']}

        if result['rc'] != 0:
            return {'status': 'failed', 'error': result.get('stderr', 'Unknown error'), 'output': result.get('stdout', '')}

        # Parse JSON result from stdout (last JSON block)
        try:
            lines = result['stdout'].split('\n')
            for line in reversed(lines):
                line = line.strip()
                if line.startswith('{'):
                    parsed = json.loads(line)
                    if 'session_id' in parsed:
                        return parsed
        except Exception:
            pass

        return {'status': 'complete', 'output': result['stdout'][:500]}

    @mcp.tool()
    def wizard_status(session_id: str) -> dict:
        """Check the status of a wizard consultation session.
        session_id: UUID from wizard_consult."""

        cmd = f'cat /opt/lumina-fleet/wizard/sessions/{session_id}.json 2>/dev/null || echo not_found'
        result = _ssh_exec(cmd, timeout=10)

        if result.get('error') or result['stdout'] == 'not_found':
            return {'status': 'not_found', 'session_id': session_id}

        try:
            return json.loads(result['stdout'])
        except Exception:
            return {'status': 'unknown', 'raw': result['stdout'][:200]}

    @mcp.tool()
    def wizard_history(
        limit: int = 10,
        topic_filter: str = '',
    ) -> dict:
        """List recent Mr. Wizard consultations stored in Engram.
        limit: max results (default 10).
        topic_filter: keyword to search in consultation titles."""

        cmd = f'ls -t /opt/lumina-fleet/wizard/sessions/*.json 2>/dev/null | head -{limit}'
        result = _ssh_exec(cmd, timeout=10)

        sessions = []
        if result.get('stdout'):
            for path in result['stdout'].split('\n'):
                path = path.strip()
                if not path: continue
                cat_result = _ssh_exec(f'cat {path} 2>/dev/null', timeout=5)
                if cat_result.get('stdout'):
                    try:
                        s = json.loads(cat_result['stdout'])
                        if topic_filter and topic_filter.lower() not in s.get('query', '').lower():
                            continue
                        sessions.append({
                            'session_id': s.get('id', '')[:8],
                            'query': s.get('query', '')[:80],
                            'tier': s.get('tier', ''),
                            'council_used': s.get('council_used', False),
                            'status': s.get('status', ''),
                            'created_at': s.get('created_at', ''),
                            'engram_path': s.get('engram_path', ''),
                        })
                    except Exception:
                        pass

        return {'count': len(sessions), 'sessions': sessions}
