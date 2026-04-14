"""
Vector core executor — creates PRs and manages git operations.
Used by VectorLoop after task completion.
"""
import os
import subprocess
import json
import logging
import urllib.request
from datetime import datetime
from pathlib import Path

log = logging.getLogger('vector.executor')

GITEA_URL = os.environ.get('GITEA_URL', 'http://YOUR_GITEA_IP:3000')
GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')


def run_cmd(cmd: str, cwd: str = None, timeout: int = 30) -> tuple[int, str, str]:
    """Run a shell command. Returns (returncode, stdout, stderr)."""
    result = subprocess.run(
        cmd, shell=True, capture_output=True, text=True,
        timeout=timeout, cwd=cwd
    )
    return result.returncode, result.stdout.strip(), result.stderr.strip()


def create_branch(repo_path: str, branch_name: str) -> bool:
    """Create and checkout a new branch for the task."""
    rc, _, err = run_cmd(f'git checkout -b {branch_name}', cwd=repo_path)
    if rc != 0:
        # Branch may exist — try to checkout
        rc2, _, _ = run_cmd(f'git checkout {branch_name}', cwd=repo_path)
        if rc2 != 0:
            log.error(f'Could not create/checkout branch {branch_name}: {err}')
            return False
    log.info(f'On branch: {branch_name}')
    return True


def commit_changes(repo_path: str, message: str, files: list = None) -> bool:
    """Stage and commit changes."""
    if files:
        for f in files:
            run_cmd(f'git add "{f}"', cwd=repo_path)
    else:
        run_cmd('git add -A', cwd=repo_path)

    rc, out, err = run_cmd(f'git commit -m "{message}"', cwd=repo_path)
    if rc != 0:
        if 'nothing to commit' in out + err:
            log.info('Nothing to commit')
            return True
        log.error(f'Commit failed: {err}')
        return False
    log.info(f'Committed: {message[:60]}')
    return True


def push_branch(repo_path: str, branch_name: str, remote: str = 'origin') -> bool:
    """Push branch to remote."""
    rc, _, err = run_cmd(f'git push -u {remote} {branch_name}', cwd=repo_path)
    if rc != 0:
        log.error(f'Push failed: {err}')
        return False
    log.info(f'Pushed {branch_name} to {remote}')
    return True


def create_pr(repo_owner: str, repo_name: str, branch_name: str,
              title: str, body: str, base: str = 'main') -> dict:
    """Create a pull request on Gitea. Returns PR data or error dict."""
    if not GITEA_TOKEN:
        return {'error': 'GITEA_TOKEN not set'}

    payload = json.dumps({
        'head': branch_name,
        'base': base,
        'title': title,
        'body': body,
    }).encode()

    req = urllib.request.Request(
        f'{GITEA_URL}/api/v1/repos/{repo_owner}/{repo_name}/pulls',
        data=payload,
        headers={
            'Authorization': f'token {GITEA_TOKEN}',
            'Content-Type': 'application/json',
        },
        method='POST'
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            pr = json.load(r)
        log.info(f'PR created: #{pr["number"]} — {pr["html_url"]}')
        return {'pr_number': pr['number'], 'url': pr['html_url'], 'title': pr['title']}
    except Exception as e:
        log.error(f'PR creation failed: {e}')
        return {'error': str(e)}


def full_pr_workflow(repo_path: str, task_name: str, changed_files: list = None,
                     repo_owner: str = 'moosenet', repo_name: str = 'lumina-fleet') -> dict:
    """
    Complete workflow: branch → commit → push → PR.
    Returns PR data or error.
    """
    # Generate branch name from task
    slug = task_name.lower()[:40].replace(' ', '-').replace('/', '-')
    timestamp = datetime.now().strftime('%Y%m%d-%H%M')
    branch = f'vector/{timestamp}-{slug}'

    # Check if there's anything to commit
    rc, status, _ = run_cmd('git status --porcelain', cwd=repo_path)
    if not status.strip():
        log.info('No changes to commit — skipping PR workflow')
        return {'skipped': True, 'reason': 'No changes detected'}

    # Get repo origin for owner/name detection
    _, remote_url, _ = run_cmd('git remote get-url origin', cwd=repo_path)
    if 'gitea' in remote_url or 'YOUR_GITEA_IP' in remote_url:
        # Extract owner/repo from URL
        parts = remote_url.rstrip('/').rstrip('.git').split('/')
        if len(parts) >= 2:
            repo_owner = parts[-2]
            repo_name = parts[-1]

    steps = {}

    if not create_branch(repo_path, branch):
        return {'error': 'Failed to create branch', 'branch': branch}
    steps['branch'] = branch

    commit_msg = f'[Vector] {task_name[:60]}\n\nAutomated change by Vector autonomous dev loop'
    if not commit_changes(repo_path, commit_msg, changed_files):
        return {'error': 'Commit failed', 'steps': steps}
    steps['committed'] = True

    if not push_branch(repo_path, branch):
        return {'error': 'Push failed', 'steps': steps}
    steps['pushed'] = True

    pr_body = f"""## Vector Automated Change

**Task:** {task_name}

**Changed files:** {', '.join(changed_files) if changed_files else 'See diff'}

---
*Created by Vector autonomous dev loop. Review before merging.*
"""
    pr = create_pr(repo_owner, repo_name, branch, f'[Vector] {task_name[:60]}', pr_body)
    steps['pr'] = pr

    if 'error' in pr:
        return {'error': f'PR failed: {pr["error"]}', 'steps': steps}

    return {
        'status': 'pr_created',
        'branch': branch,
        'pr_url': pr.get('url'),
        'pr_number': pr.get('pr_number'),
        'steps': steps
    }
