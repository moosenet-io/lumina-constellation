import subprocess, json, os

DURA_HOST = 'root@YOUR_FLEET_SERVER_IP'
DURA_DIR = '/opt/lumina-fleet/dura'

def _dura(cmd, timeout=30):
    full = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {DURA_HOST} '{cmd}'"
    try:
        r = subprocess.run(full, shell=True, capture_output=True, text=True, timeout=timeout)
        if r.returncode != 0:
            return {'error': r.stderr.strip()[:200] or r.stdout.strip()[:200]}
        try:
            return json.loads(r.stdout.strip())
        except Exception:
            return {'output': r.stdout.strip()[:500]}
    except Exception as e:
        return {'error': str(e)}

def register_dura_tools(mcp):
    @mcp.tool()
    def dura_backup_status() -> dict:
        """Get status of last Dura backup. Returns {last_run, files_backed_up, success}"""
        cmd = f"cat {DURA_DIR}/output/backup_status.json 2>/dev/null || echo '{{\"note\":\"No backup run yet\"}}'"
        return _dura(cmd)

    @mcp.tool()
    def dura_run_backup(mode: str = 'hourly') -> dict:
        """Trigger backup run. mode: hourly (critical DBs) or daily (full)."""
        src = f"source {DURA_DIR}/../axon/.env"
        cmd = f"{src} && python3 {DURA_DIR}/dura_backup.py {mode} 2>&1 | tail -20"
        return _dura(cmd, timeout=120)

    @mcp.tool()
    def dura_smoke_test(quick: bool = True) -> dict:
        """Run MCP tool smoke tests. quick=True skips slow tests. Returns {passed, failed, failures}"""
        flag = '--quick' if quick else ''
        src = f"source {DURA_DIR}/../axon/.env"
        cmd = f"{src} && python3 {DURA_DIR}/dura_smoke_test.py {flag} 2>&1 | tail -30"
        result = _dura(cmd, timeout=300)
        status = _dura(f"cat {DURA_DIR}/output/smoke_test_results.json 2>/dev/null || echo '{{}}'")
        return status if isinstance(status, dict) and 'total' in status else result

    @mcp.tool()
    def dura_smoke_status() -> dict:
        """Get results of last smoke test run."""
        cmd = f"cat {DURA_DIR}/output/smoke_test_results.json 2>/dev/null || echo '{{\"note\":\"No smoke test yet\"}}'"
        return _dura(cmd)

    @mcp.tool()
    def dura_backup_list() -> dict:
        """List available backup files on NFS or local."""
        nfs = _dura("ls /mnt/backup 2>/dev/null && echo OK || echo FAIL")
        bdir = '/mnt/backup' if nfs.get('output','') == 'OK' else f'{DURA_DIR}/backups'
        result = _dura(f"ls -lh {bdir}/ 2>/dev/null | head -30 || echo 'Not accessible'")
        return {'backup_dir': bdir, 'listing': result.get('output', str(result))}

    @mcp.tool()
    def dura_log_query(query: str = 'error', source: str = 'fleet-host', lines: int = 20) -> dict:
        """Search journald logs. source: ironclaw-host|fleet-host|postgres-host|terminus-host|litellm-host"""
        remote_host = os.environ.get('REMOTE_SSH_HOST', '')
        template = os.environ.get('REMOTE_EXEC_TEMPLATE', '')
        targets = {
            'ironclaw-host': os.environ.get('IRONCLAW_REMOTE_TARGET', ''),
            'fleet-host': os.environ.get('FLEET_REMOTE_TARGET', ''),
            'postgres-host': os.environ.get('POSTGRES_REMOTE_TARGET', ''),
            'terminus-host': os.environ.get('TERMINUS_REMOTE_TARGET', ''),
            'litellm-host': os.environ.get('LITELLM_REMOTE_TARGET', ''),
        }
        target = targets.get(source, targets['fleet-host'])
        if not (remote_host and target and template):
            return {'error': 'remote access not configured'}
        remote_cmd = template.format(
            target=target,
            command=f"journalctl -n 200 --no-pager 2>/dev/null | grep -i \"{query}\" | tail -{lines}",
        )
        cmd = ['ssh', remote_host, remote_cmd]
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=15)
            matches = [l for l in r.stdout.splitlines() if query.lower() in l.lower()]
            return {'source': source, 'query': query, 'match_count': len(matches), 'matches': matches[-lines:]}
        except Exception as e:
            return {'error': str(e)[:100]}

    @mcp.tool()
    def dura_constellation_health() -> dict:
        """Full health — backup status, smoke tests, NFS."""
        bk = _dura(f"cat {DURA_DIR}/output/backup_status.json 2>/dev/null || echo '{{}}'")
        sm = _dura(f"cat {DURA_DIR}/output/smoke_test_results.json 2>/dev/null || echo '{{}}'")
        nfs = _dura("mount | grep nfs | head -1 || echo 'not mounted'")
        return {
            'backup_ok': bk.get('success', False) if isinstance(bk, dict) else False,
            'smoke_ok': sm.get('failed', 1) == 0 if isinstance(sm, dict) else False,
            'nfs_mounted': 'nfs' in str(nfs).lower(),
            'backup': bk, 'smoke': {k:v for k,v in sm.items() if k!='tool_results'} if isinstance(sm,dict) else sm,
        }
