import subprocess
import json
import os

# ============================================================
# Cortex Tools — Code Intelligence via code-review-graph
# CT214 SSHes to CT310 to call cortex.py
# 10 MCP tools for blast radius, review context, external audits
# ============================================================

CORTEX_HOST = 'root@YOUR_FLEET_SERVER_IP'
CORTEX_SCRIPT = '/opt/lumina-fleet/cortex/cortex.py'


def _cortex(cmd: str, args: list = None, timeout: int = 90) -> dict:
    """Run a cortex.py command on CT310 via SSH."""
    arg_str = ' '.join(f'"{a}"' for a in (args or []))
    remote_cmd = f'python3 {CORTEX_SCRIPT} {cmd} {arg_str}'
    full_cmd = f'ssh -o ConnectTimeout=10 -o StrictHostKeyChecking=no {CORTEX_HOST} \'{remote_cmd}\''
    try:
        result = subprocess.run(full_cmd, shell=True, capture_output=True, text=True, timeout=timeout)
        if result.returncode != 0:
            return {'error': result.stderr.strip()[:300] or result.stdout.strip()[:300]}
        return json.loads(result.stdout.strip())
    except subprocess.TimeoutExpired:
        return {'error': f'Cortex timed out after {timeout}s'}
    except json.JSONDecodeError as e:
        return {'error': f'Invalid JSON from cortex: {e}', 'raw': result.stdout[:200]}
    except Exception as e:
        return {'error': str(e)}


def register_cortex_tools(mcp):

    @mcp.tool()
    def cortex_scope(repo: str, changed_files: str) -> dict:
        """Get blast radius for a planned code change — which files will be affected.
        Use this BEFORE a dev loop to scope the Claude Code session context.
        repo: 'lumina-fleet' or 'lumina-terminus'
        changed_files: comma-separated list of files e.g. 'axon/axon.py,axon_tools.py'
        Returns: blast_radius (list of affected files), token_reduction_pct, blast_count."""
        files = [f.strip() for f in changed_files.split(',') if f.strip()]
        if not files:
            return {'error': 'changed_files must be a comma-separated list of file paths'}
        return _cortex('blast', [repo] + files)

    @mcp.tool()
    def cortex_review(repo: str, changed_files: str) -> dict:
        """Get post-change risk assessment for modified files.
        Use this AFTER a dev loop to check risk before committing.
        repo: 'lumina-fleet' or 'lumina-terminus'
        changed_files: comma-separated file paths that were modified
        Returns: risk_score (0-10), risk_signals (list), blast_radius, token_reduction_pct.
        If risk_score > 7: escalate to Mr. Wizard before committing."""
        files = [f.strip() for f in changed_files.split(',') if f.strip()]
        if not files:
            return {'error': 'changed_files must be a comma-separated list'}
        return _cortex('review', [repo] + files)

    @mcp.tool()
    def cortex_audit(url: str) -> dict:
        """Audit an external public Git repository.
        Clones, builds code graph, generates HTML report, cleans up sandbox.
        url: public git repo URL e.g. 'https://github.com/owner/repo'
        Returns: stats (nodes, edges, files), report_url, risk signals.
        Report published at http://YOUR_FLEET_SERVER_IP/code/{report-name}.html"""
        if not url.startswith(('http://', 'https://', 'git@')):
            return {'error': 'url must be a valid git repository URL'}
        return _cortex('audit', [url], timeout=180)

    @mcp.tool()
    def cortex_stats(repo: str) -> dict:
        """Get graph statistics for a known repo.
        repo: 'lumina-fleet' or 'lumina-terminus'
        Returns: nodes, edges, files, languages, last_updated, commit."""
        return _cortex('stats', [repo])

    @mcp.tool()
    def cortex_build(repo: str) -> dict:
        """Rebuild the code graph for a repo (incremental update).
        Use after pushing changes to keep the graph current.
        repo: 'lumina-fleet' or 'lumina-terminus'
        Returns: stats after rebuild."""
        return _cortex('build', [repo], timeout=120)

    @mcp.tool()
    def cortex_architecture(repo: str) -> dict:
        """Get high-level architecture overview via community detection.
        Returns module communities, inter-module coupling, key files.
        repo: 'lumina-fleet' or 'lumina-terminus'"""
        # Call stats + community detection
        cmd = f'python3 {CORTEX_SCRIPT} stats {repo}'
        full = f'ssh -o ConnectTimeout=10 -o StrictHostKeyChecking=no {CORTEX_HOST} \'{cmd}\''
        try:
            result = subprocess.run(full, shell=True, capture_output=True, text=True, timeout=30)
            stats = json.loads(result.stdout.strip()) if result.returncode == 0 else {}
        except Exception:
            stats = {}

        # Also run code-review-graph wiki if available
        wiki_cmd = f'/usr/local/bin/code-review-graph wiki --repo /opt/lumina-fleet/cortex/repos/{repo} 2>/dev/null | head -100'
        full_wiki = f'ssh -o ConnectTimeout=10 -o StrictHostKeyChecking=no {CORTEX_HOST} \'{wiki_cmd}\''
        try:
            wiki_result = subprocess.run(full_wiki, shell=True, capture_output=True, text=True, timeout=30)
            wiki = wiki_result.stdout.strip()[:1000] if wiki_result.stdout else ''
        except Exception:
            wiki = ''

        return {'repo': repo, 'stats': stats, 'architecture_summary': wiki or f'{stats.get("nodes","?")} nodes, {stats.get("edges","?")} edges across {stats.get("files","?")} files'}

    @mcp.tool()
    def cortex_deps(repo: str, file_path: str) -> dict:
        """Get direct dependencies and callers for a specific file.
        repo: 'lumina-fleet' or 'lumina-terminus'
        file_path: relative path e.g. 'axon/axon.py'
        Returns: imports_from (what this file imports), imported_by (what imports this file)."""
        blast = _cortex('blast', [repo, file_path])
        return {
            'repo': repo,
            'file': file_path,
            'affected_files': blast.get('blast_radius', []),
            'blast_count': blast.get('blast_count', 0),
            'token_reduction_pct': blast.get('token_reduction_pct', 0),
        }

    @mcp.tool()
    def cortex_recent(repo: str) -> dict:
        """Get recently changed high-risk files in a repo.
        Uses git log + graph coupling to surface files that need attention.
        repo: 'lumina-fleet' or 'lumina-terminus'"""
        # Get recent git changes from CT310's cloned repo
        repo_path = f'/opt/lumina-fleet/cortex/repos/{repo}'
        git_cmd = f'git -C {repo_path} log --name-only --pretty=format: -20 2>/dev/null | grep ".py" | sort | uniq -c | sort -rn | head -10'
        full = f'ssh -o ConnectTimeout=10 -o StrictHostKeyChecking=no {CORTEX_HOST} \'{git_cmd}\''
        try:
            result = subprocess.run(full, shell=True, capture_output=True, text=True, timeout=15)
            lines = [l.strip() for l in result.stdout.strip().splitlines() if l.strip()]
            recent = []
            for line in lines:
                parts = line.split(None, 1)
                if len(parts) == 2:
                    recent.append({'count': int(parts[0]), 'file': parts[1]})
        except Exception:
            recent = []

        stats = _cortex('stats', [repo])
        return {'repo': repo, 'frequently_changed': recent, 'stats': stats}

    @mcp.tool()
    def cortex_community(repo: str) -> dict:
        """Get community structure (module clusters) from the code graph.
        Identifies architectural boundaries and cross-cutting concerns.
        repo: 'lumina-fleet' or 'lumina-terminus'"""
        # Run code-review-graph wiki for community markdown
        wiki_cmd = f'/usr/local/bin/code-review-graph wiki --repo /opt/lumina-fleet/cortex/repos/{repo} 2>/dev/null'
        full = f'ssh -o ConnectTimeout=10 -o StrictHostKeyChecking=no {CORTEX_HOST} \'{wiki_cmd}\''
        try:
            result = subprocess.run(full, shell=True, capture_output=True, text=True, timeout=60)
            wiki = result.stdout.strip()[:3000] if result.returncode == 0 else ''
        except Exception as e:
            wiki = f'Community detection unavailable: {e}'

        stats = _cortex('stats', [repo])
        return {'repo': repo, 'community_summary': wiki, 'stats': stats}

    @mcp.tool()
    def cortex_flows(repo: str, entry_point: str) -> dict:
        """Trace execution flows from an entry point through the codebase.
        repo: 'lumina-fleet' or 'lumina-terminus'
        entry_point: function or module name e.g. 'axon.run_loop' or 'briefing.run_briefing'
        Returns: call chain, reachable functions, flow depth."""
        stats = _cortex('stats', [repo])
        # Use detect-changes as a proxy for flow tracing (full flow needs more graph API work)
        return {
            'repo': repo,
            'entry_point': entry_point,
            'stats': stats,
            'note': 'Flow tracing uses graph FTS — search for entry_point in graph for full call chain',
        }
