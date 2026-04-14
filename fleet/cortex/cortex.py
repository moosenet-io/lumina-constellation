#!/usr/bin/env python3
"""
Cortex — Code Intelligence Engine for Lumina Constellation.
Wraps code-review-graph (Tree-sitter AST) for blast-radius analysis,
structural review, and external repo auditing.

CT310 /opt/lumina-fleet/cortex/cortex.py
"""

import os
import sys
import json
import subprocess
import shutil
import hashlib
import logging
from pathlib import Path
from datetime import datetime
from typing import Optional

log = logging.getLogger('cortex')
logging.basicConfig(level=logging.INFO, format='%(asctime)s [cortex] %(levelname)s %(message)s')

CRG = '/usr/local/bin/code-review-graph'
REPOS_DIR = Path('/opt/lumina-fleet/cortex/repos')
SANDBOX_DIR = Path('/tmp/cortex-sandbox')
REPORTS_DIR = Path('/opt/lumina-fleet/cortex/reports')
LOGS_DIR = Path('/opt/lumina-fleet/cortex/logs')

KNOWN_REPOS = {
    'lumina-fleet': str(REPOS_DIR / 'lumina-fleet'),
    'lumina-terminus': str(REPOS_DIR / 'lumina-terminus'),
}

GITEA_URL = os.environ.get('GITEA_URL', 'http://YOUR_GITEA_IP:3000')
GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')


def _crg(args: list, repo_path: str = None, timeout: int = 60) -> dict:
    """Run code-review-graph and return parsed output."""
    cmd = [CRG] + args
    if repo_path:
        cmd += ['--repo', repo_path]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return {
            'stdout': result.stdout.strip(),
            'stderr': result.stderr.strip(),
            'returncode': result.returncode,
        }
    except subprocess.TimeoutExpired:
        return {'error': f'Timeout after {timeout}s', 'returncode': -1}
    except Exception as e:
        return {'error': str(e), 'returncode': -1}


def _resolve_repo(repo: str) -> Optional[str]:
    """Resolve repo name to local path."""
    if repo in KNOWN_REPOS:
        return KNOWN_REPOS[repo]
    p = Path(repo)
    if p.exists() and (p / '.code-review-graph').exists():
        return str(p)
    return None


def get_stats(repo: str) -> dict:
    """Return graph statistics for a repo."""
    path = _resolve_repo(repo)
    if not path:
        return {'error': f'Unknown repo: {repo}. Known: {list(KNOWN_REPOS.keys())}'}
    result = _crg(['status'], repo_path=path)
    if result.get('returncode', 1) != 0:
        return {'error': result.get('stderr', 'stats failed')}
    stats = {}
    for line in result['stdout'].splitlines():
        if ':' in line:
            k, v = line.split(':', 1)
            stats[k.strip().lower().replace(' ', '_')] = v.strip()
    stats['repo'] = repo
    return stats


def get_blast_radius(repo: str, changed_files: list) -> dict:
    """
    Get blast radius for a set of changed files.
    Returns: affected files, callers, dependents, estimated review scope.
    """
    path = _resolve_repo(repo)
    if not path:
        return {'error': f'Unknown repo: {repo}'}

    try:
        from code_review_graph.incremental import GraphStore, get_db_path, find_dependents
        db_path = get_db_path(Path(path))
        store = GraphStore(str(db_path))

        affected = set(changed_files)
        for f in changed_files:
            dependents = find_dependents(store, f, max_hops=2)
            affected.update(dependents)
        store.close()
        blast = sorted(affected)
    except Exception as e:
        log.warning(f'Blast radius API failed: {e}, using changed files only')
        blast = list(changed_files)

    stats = get_stats(repo)
    total_files = int(stats.get('files', 0)) or 1
    reduction = round((1 - len(blast) / total_files) * 100, 1) if total_files > len(blast) else 0

    return {
        'repo': repo,
        'changed_files': changed_files,
        'blast_radius': blast,
        'blast_count': len(blast),
        'total_files': total_files,
        'token_reduction_pct': reduction,
    }


def get_review_context(repo: str, changed_files: list) -> dict:
    """
    Get review context for a set of files — architecture summary + risk signals.
    Used by Vector post-flight and Mr. Wizard consultations.
    """
    path = _resolve_repo(repo)
    if not path:
        return {'error': f'Unknown repo: {repo}'}

    blast = get_blast_radius(repo, changed_files)
    stats = get_stats(repo)

    # Get risk signals from graph
    risk_signals = []
    risk_score = 0

    try:
        from code_review_graph.incremental import GraphStore, get_db_path
        from code_review_graph.changes import analyze_changes, compute_risk_score
        db_path = get_db_path(Path(path))
        store = GraphStore(str(db_path))
        analysis = analyze_changes(store, changed_files, repo_root=path)
        # Extract risk signals from analysis
        for node_info in analysis.get('affected_nodes', [])[:10]:
            node_risk = compute_risk_score(store, node_info) if hasattr(node_info, '__dict__') else 0
            if node_risk > 5:
                risk_signals.append(f'High risk node: {getattr(node_info, "file_path", str(node_info))}')
                risk_score += 2
        store.close()
    except Exception as e:
        log.debug(f'Risk analysis failed: {e}')

    # Heuristic risk: many files affected = higher risk
    if blast.get('blast_count', 0) > 10:
        risk_score += 3
        risk_signals.append(f'Wide blast radius: {blast["blast_count"]} files affected')
    elif blast.get('blast_count', 0) > 5:
        risk_score += 1

    risk_score = min(10, risk_score)

    return {
        'repo': repo,
        'changed_files': changed_files,
        'blast_radius': blast.get('blast_radius', changed_files),
        'risk_score': risk_score,
        'risk_signals': risk_signals,
        'stats': stats,
        'review_scope': blast.get('blast_count', len(changed_files)),
        'token_reduction_pct': blast.get('token_reduction_pct', 0),
    }


def get_architecture(repo: str) -> dict:
    """Get high-level architecture overview from community detection."""
    path = _resolve_repo(repo)
    if not path:
        return {'error': f'Unknown repo: {repo}'}

    result = _crg(['wiki', '--format', 'json'] if False else ['status'], repo_path=path)
    stats = get_stats(repo)

    # Try to get community info from DB
    communities = []
    try:
        from code_review_graph.incremental import GraphStore, get_db_path
        from code_review_graph.communities import detect_communities
        db_path = get_db_path(Path(path))
        store = GraphStore(str(db_path))
        communities = detect_communities(store) if callable(getattr(__import__('code_review_graph.communities', fromlist=['detect_communities']), 'detect_communities', None)) else []
        store.close()
    except Exception as e:
        log.debug(f'Community detection failed: {e}')

    return {
        'repo': repo,
        'stats': stats,
        'communities': communities,
        'community_count': len(communities),
    }


def build_graph(repo: str, force: bool = False) -> dict:
    """Build or rebuild the graph for a known repo."""
    path = _resolve_repo(repo)
    if not path:
        # Try to clone from Gitea
        if repo in ('lumina-fleet', 'lumina-terminus'):
            clone_url = f'{GITEA_URL}/moosenet/{repo}.git'
            if GITEA_TOKEN:
                clone_url = f'http://{GITEA_TOKEN}@{GITEA_URL.split("//")[1]}/moosenet/{repo}.git'
            dest = str(REPOS_DIR / repo)
            rc = subprocess.run(['git', 'clone', clone_url, dest], capture_output=True, timeout=60).returncode
            if rc != 0:
                return {'error': f'Could not clone {repo}'}
            path = dest
            KNOWN_REPOS[repo] = path
        else:
            return {'error': f'Unknown repo: {repo}'}

    cmd = ['build'] if force else ['update']
    result = _crg(cmd, repo_path=path, timeout=120)
    if result.get('returncode', 1) != 0:
        # Try full build if update fails
        result = _crg(['build'], repo_path=path, timeout=180)

    stats = get_stats(repo)
    return {
        'repo': repo,
        'action': 'built',
        'stats': stats,
        'output': result.get('stdout', '')[-200:],
    }


def sandbox_clone(url: str) -> dict:
    """Clone an external repo to sandbox. Returns sandbox path or error."""
    SANDBOX_DIR.mkdir(parents=True, exist_ok=True)

    # Hash the URL to create unique sandbox dir
    url_hash = hashlib.sha256(url.encode()).hexdigest()[:12]
    repo_name = url.rstrip('/').split('/')[-1].replace('.git', '')
    sandbox_path = SANDBOX_DIR / f'{repo_name}-{url_hash}'

    if sandbox_path.exists():
        shutil.rmtree(sandbox_path)

    try:
        result = subprocess.run(
            ['git', 'clone', '--depth', '1', url, str(sandbox_path)],
            capture_output=True, text=True, timeout=60
        )
        if result.returncode != 0:
            return {'error': f'Clone failed: {result.stderr[:200]}'}
    except subprocess.TimeoutExpired:
        return {'error': 'Clone timed out (60s)'}
    except Exception as e:
        return {'error': str(e)}

    return {'path': str(sandbox_path), 'repo_name': repo_name, 'url_hash': url_hash}


def sandbox_cleanup(path: str) -> bool:
    """Remove sandbox directory. Only deletes paths under /tmp/cortex-sandbox/."""
    p = Path(path)
    if not str(p).startswith(str(SANDBOX_DIR)):
        log.warning(f'Refusing to delete outside sandbox: {path}')
        return False
    try:
        if p.exists():
            shutil.rmtree(p)
        return True
    except Exception as e:
        log.error(f'Cleanup failed: {e}')
        return False


def audit_repo(url: str) -> dict:
    """
    Full external repo audit. Clones, builds graph, generates report.
    Returns report path and summary stats.
    """
    log.info(f'Starting audit: {url}')

    clone = sandbox_clone(url)
    if 'error' in clone:
        return clone

    sandbox_path = clone['path']
    repo_name = clone['repo_name']

    try:
        # Build graph in sandbox
        result = _crg(['build', '--skip-flows'], repo_path=sandbox_path, timeout=180)
        if result.get('returncode', 1) != 0:
            sandbox_cleanup(sandbox_path)
            return {'error': f'Graph build failed: {result.get("stderr","")[:200]}'}

        # Get stats
        stats_result = _crg(['status'], repo_path=sandbox_path)
        stats = {}
        for line in stats_result.get('stdout', '').splitlines():
            if ':' in line:
                k, v = line.split(':', 1)
                stats[k.strip().lower().replace(' ', '_')] = v.strip()

        # Generate report
        timestamp = datetime.now().strftime('%Y%m%d-%H%M%S')
        report_name = f'{repo_name}-{timestamp}'
        report = _generate_report(repo_name, url, stats, sandbox_path, report_name)

    finally:
        sandbox_cleanup(sandbox_path)

    return {
        'repo': repo_name,
        'url': url,
        'stats': stats,
        'report_name': report_name,
        'report_url': f'http://YOUR_FLEET_SERVER_IP/code/{report_name}.html',
        'report_path': report,
    }


def _generate_report(repo_name: str, url: str, stats: dict, repo_path: str, report_name: str) -> str:
    """Generate HTML audit report."""
    REPORTS_DIR.mkdir(parents=True, exist_ok=True)
    report_path = REPORTS_DIR / f'{report_name}.html'

    nodes = stats.get('nodes', 'N/A')
    edges = stats.get('edges', 'N/A')
    files = stats.get('files', 'N/A')
    langs = stats.get('languages', 'N/A')

    html_out = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Cortex Audit — {repo_name}</title>
<link rel="stylesheet" href="/shared/constellation.css">
</head>
<body>
<div class="lumina-header">
    <div class="lumina-logo">Cortex Audit Report</div>
    <div class="lumina-subtitle">{repo_name}</div>
</div>
<div class="page">
    <div class="card" style="margin-bottom:var(--space-4)">
        <div class="card-header">Repository</div>
        <p style="margin-bottom:var(--space-2)"><a href="{url}" style="color:var(--accent-text)">{url}</a></p>
        <p class="text-secondary" style="font-size:var(--text-sm)">Analyzed: {datetime.now().strftime('%Y-%m-%d %H:%M UTC')}</p>
    </div>

    <h2 class="section-title">Graph Statistics</h2>
    <div class="grid-4" style="margin-bottom:var(--space-5)">
        <div class="card stat-card">
            <div class="stat-value">{nodes}</div>
            <div class="stat-label">Nodes</div>
        </div>
        <div class="card stat-card">
            <div class="stat-value">{edges}</div>
            <div class="stat-label">Edges</div>
        </div>
        <div class="card stat-card">
            <div class="stat-value">{files}</div>
            <div class="stat-label">Files</div>
        </div>
        <div class="card stat-card">
            <div class="stat-value">{langs}</div>
            <div class="stat-label">Languages</div>
        </div>
    </div>

    <div class="card" style="margin-bottom:var(--space-4)">
        <div class="card-header">Analysis</div>
        <p class="text-secondary">Graph built successfully. Full community detection and flow analysis available via <code>cortex_architecture</code> MCP tool.</p>
    </div>

    <div class="card">
        <div class="card-header">Security Notes</div>
        <ul style="padding-left:var(--space-4);color:var(--text-secondary);font-size:var(--text-sm);line-height:var(--leading-normal)">
            <li>Repository cloned to isolated sandbox and deleted after analysis</li>
            <li>No code was executed from the analyzed repository</li>
            <li>Analysis performed via Tree-sitter AST parsing only</li>
        </ul>
    </div>
</div>
<div class="lumina-footer">
    Generated by Cortex · Lumina Constellation Module 15 · MooseNet<br>
    Powered by <a href="https://github.com/tirth8205/code-review-graph" style="color:var(--text-tertiary)">code-review-graph</a> (MIT)
</div>
</body>
</html>"""

    report_path.write_text(html_out)
    log.info(f'Report written: {report_path}')
    return str(report_path)


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description='Cortex code intelligence engine')
    sub = parser.add_subparsers(dest='cmd')

    p = sub.add_parser('stats'); p.add_argument('repo')
    p = sub.add_parser('blast'); p.add_argument('repo'); p.add_argument('files', nargs='+')
    p = sub.add_parser('review'); p.add_argument('repo'); p.add_argument('files', nargs='+')
    p = sub.add_parser('build'); p.add_argument('repo')
    p = sub.add_parser('audit'); p.add_argument('url')

    args = parser.parse_args()
    if not args.cmd:
        parser.print_help(); sys.exit(0)

    if args.cmd == 'stats':
        print(json.dumps(get_stats(args.repo), indent=2))
    elif args.cmd == 'blast':
        print(json.dumps(get_blast_radius(args.repo, args.files), indent=2))
    elif args.cmd == 'review':
        print(json.dumps(get_review_context(args.repo, args.files), indent=2))
    elif args.cmd == 'build':
        print(json.dumps(build_graph(args.repo), indent=2))
    elif args.cmd == 'audit':
        print(json.dumps(audit_repo(args.url), indent=2))
