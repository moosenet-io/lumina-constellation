import subprocess
import json

# ============================================================
# Seer Tools — Research Engine
# MCP tools that trigger Seer research jobs on CT310.
# ============================================================

SEER_HOST = 'root@YOUR_FLEET_SERVER_IP'
SEER_SCRIPT = '/usr/bin/python3 /opt/lumina-fleet/seer/seer.py'


def _ssh_exec(cmd, timeout=600):
    full_cmd = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {SEER_HOST} '{cmd}'"
    try:
        result = subprocess.run(full_cmd, shell=True, capture_output=True, text=True, timeout=timeout)
        return {'stdout': result.stdout.strip(), 'stderr': result.stderr.strip(), 'rc': result.returncode}
    except subprocess.TimeoutExpired:
        return {'error': f'Seer timed out ({timeout}s)', 'rc': -1}
    except Exception as e:
        return {'error': str(e), 'rc': -1}


def register_seer_tools(mcp):

    @mcp.tool()
    def seer_research(query: str, effort: str = 'standard', focus: str = 'overview', max_sources: int = 0) -> dict:
        """Research a topic using Seer — multi-source web research with injection sanitization.
        query: research question or topic.
        effort: light ($0, 30s), standard ($0.01-0.03, 2min), deep ($2-5, 5-10min Sonnet synthesis).
        focus: overview, comparison, how-to, opinion.
        Returns: {report_id, status, url, summary, sources_used}. HTML at http://YOUR_FLEET_SERVER_IP/research/"""
        if effort not in ('light', 'standard', 'deep'):
            return {'error': f'effort must be light/standard/deep, got: {effort}'}
        cmd = (f'source /opt/lumina-fleet/axon/.env && export LITELLM_MASTER_KEY GITEA_TOKEN && '
               f'cd /opt/lumina-fleet/seer && '
               f'SEARXNG_URL=http://YOUR_SEARXNG_IP:8088 {SEER_SCRIPT} '
               f'--query "{query[:400]}" --effort {effort} --focus {focus}')
        if max_sources > 0:
            cmd += f' --max-sources {max_sources}'
        result = _ssh_exec(cmd)
        if result.get('error'):
            return {'status': 'failed', 'error': result['error']}
        if result['rc'] != 0:
            return {'status': 'failed', 'error': result.get('stderr', '')[:300]}
        for line in reversed(result['stdout'].split('\n')):
            line = line.strip()
            if line.startswith('{'):
                try:
                    parsed = json.loads(line)
                    if 'report_id' in parsed:
                        return parsed
                except Exception:
                    pass
        return {'status': 'complete', 'output': result['stdout'][-400:]}

    @mcp.tool()
    def seer_status(report_id: str) -> dict:
        """Check status of a Seer research job by report_id."""
        result = _ssh_exec(f'cat /opt/lumina-fleet/seer/output/status/{report_id}.json 2>/dev/null || echo not_found', timeout=10)
        if result.get('error') or result['stdout'] == 'not_found':
            return {'status': 'not_found', 'report_id': report_id}
        try:
            return json.loads(result['stdout'])
        except Exception:
            return {'status': 'unknown', 'raw': result['stdout'][:200]}

    @mcp.tool()
    def seer_list(limit: int = 10) -> dict:
        """List recent Seer research reports. Returns list with dates, queries, URLs."""
        result = _ssh_exec(f'ls -t /opt/lumina-fleet/seer/output/markdown/*.md 2>/dev/null | head -{limit}', timeout=10)
        if not result.get('stdout'):
            return {'count': 0, 'reports': []}
        reports = []
        for path in result['stdout'].split('\n'):
            path = path.strip()
            if not path:
                continue
            name = path.split('/')[-1].replace('.md', '')
            reports.append({'filename': name, 'url': f'http://YOUR_FLEET_SERVER_IP/research/{name}.html', 'markdown': path})
        return {'count': len(reports), 'reports': reports}
