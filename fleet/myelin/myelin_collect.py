#!/usr/bin/env python3
"""
Myelin collection daemon — runs every 30 min via systemd timer.
Reads LiteLLM spend logs, OpenRouter credits, writes to myelin.db.
"""
import os, sys, json, sqlite3, urllib.request, urllib.error
from datetime import datetime, timedelta
from pathlib import Path

sys.path.insert(0, '/opt/lumina-fleet')
try: from naming import display_name as _dn
except: _dn = lambda x: x

DB_PATH = os.environ.get('MYELIN_DB_PATH', '/opt/lumina-fleet/myelin/myelin.db')
OUTPUT_PATH = Path('/opt/lumina-fleet/myelin/output/usage.json')
OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)

LITELLM_URL = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
OPENROUTER_KEY = os.environ.get('OPENROUTER_API_KEY', '')


def _http_get(url, headers=None):
    req = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.load(r)
    except Exception as e:
        return {'error': str(e)}


def collect_litellm_spend():
    """Get recent LiteLLM spend logs for per-agent attribution."""
    if not LITELLM_KEY:
        return []

    # Try LiteLLM spend logs endpoint
    data = _http_get(f'{LITELLM_URL}/spend/logs',
                     headers={'Authorization': f'Bearer {LITELLM_KEY}'})

    if 'error' in data:
        # Try alternate endpoint
        data = _http_get(f'{LITELLM_URL}/global/spend/logs',
                        headers={'Authorization': f'Bearer {LITELLM_KEY}'})

    if isinstance(data, list):
        return data[-100:]  # Last 100 entries
    return data.get('response', data.get('data', []))


def collect_openrouter_credits():
    """Get OpenRouter balance."""
    if not OPENROUTER_KEY:
        return {'error': 'no OPENROUTER_API_KEY', 'balance': None}

    data = _http_get('https://openrouter.ai/api/v1/auth/key',
                     headers={'Authorization': f'Bearer {OPENROUTER_KEY}'})

    if 'data' in data:
        d = data['data']
        return {
            'balance': d.get('limit_remaining', d.get('balance', 0)),
            'usage': d.get('usage', 0),
            'rate_limit_requests': d.get('rate_limit', {}).get('requests', 0),
        }
    return data


def collect_oauth_session_estimate():
    """
    Estimate Claude Max (OAuth CLI) session usage.

    Claude Code CLI does not expose token usage programmatically.
    We estimate via:
    1. Session log file analysis (timing + stdout char count as proxy)
    2. Manual checkpoints stored in Engram when the operator reports %

    All Claude Max estimates are clearly labeled 'estimated' vs
    OpenRouter/LiteLLM which are 'measured' from actual API responses.
    """
    estimates = {
        'provider': 'anthropic',
        'plan': 'max_5x',
        'measurement_type': 'estimated',  # Not measured — estimation only
        'confidence': 'low',
        'note': 'Claude Max usage cannot be read programmatically. Estimates from session timing only.',
    }

    # Try to read Claude Code session logs for timing data
    session_log = Path('/home/coder/.claude/projects')
    if session_log.exists():
        try:
            import glob
            # Count recent .jsonl files as proxy for session activity
            jsonl_files = list(session_log.glob('*/*.jsonl'))
            recent_files = [f for f in jsonl_files
                          if (datetime.utcnow().timestamp() - f.stat().st_mtime) < 86400]
            estimates['sessions_today_estimate'] = len(recent_files)

            # Estimate tokens from total file sizes (rough proxy: ~4 chars per token)
            total_chars = sum(f.stat().st_size for f in recent_files if f.stat().st_size < 10_000_000)
            estimates['estimated_tokens'] = total_chars // 4
            estimates['estimated_cost_usd'] = 0  # Covered by Max subscription
        except Exception as e:
            estimates['estimation_error'] = str(e)[:80]

    return estimates


def collect_local_usage():
    """Estimate local Ollama usage from LiteLLM model stats."""
    data = _http_get(f'{LITELLM_URL}/model/metrics',
                    headers={'Authorization': f'Bearer {LITELLM_KEY}'})
    local_models = {}
    if isinstance(data, dict):
        for model, metrics in data.items():
            if any(x in model.lower() for x in ['qwen', 'ollama', 'local', 'deepseek', 'llama']):
                local_models[model] = metrics
    return local_models


def compute_daily_stats(conn):
    """Compute today's spend and token counts from usage_log."""
    today = datetime.utcnow().date().isoformat()
    cur = conn.cursor()

    cur.execute("""
        SELECT
            COALESCE(SUM(cost_usd), 0) as total_cost,
            COALESCE(SUM(input_tokens + output_tokens), 0) as total_tokens,
            COUNT(*) as call_count
        FROM usage_log
        WHERE timestamp >= ?
    """, (f"{today}T00:00:00",))

    row = cur.fetchone()
    daily = {'cost_usd': round(row[0], 4), 'tokens': row[1], 'calls': row[2]}

    # Per-agent breakdown
    cur.execute("""
        SELECT agent_id,
               COALESCE(SUM(cost_usd), 0) as cost,
               COALESCE(SUM(input_tokens + output_tokens), 0) as tokens
        FROM usage_log
        WHERE timestamp >= ?
        GROUP BY agent_id
        ORDER BY cost DESC
    """, (f"{today}T00:00:00",))

    per_agent = {row[0]: {'cost': round(row[1], 4), 'tokens': row[2]}
                 for row in cur.fetchall()}

    return daily, per_agent


def update_agent_baselines(conn):
    """Update 14-day rolling averages for each agent."""
    cutoff = (datetime.utcnow() - timedelta(days=14)).isoformat()
    cur = conn.cursor()

    cur.execute("""
        SELECT agent_id,
               AVG(daily_cost) as avg_cost,
               AVG(daily_tokens) as avg_tokens
        FROM (
            SELECT agent_id,
                   DATE(timestamp) as day,
                   SUM(cost_usd) as daily_cost,
                   SUM(input_tokens + output_tokens) as daily_tokens
            FROM usage_log
            WHERE timestamp >= ?
            GROUP BY agent_id, DATE(timestamp)
        )
        GROUP BY agent_id
    """, (cutoff,))

    for row in cur.fetchall():
        agent_id, avg_cost, avg_tokens = row
        cur.execute("""
            INSERT OR REPLACE INTO agent_baselines
                (agent_id, avg_daily_cost, avg_daily_tokens, updated)
            VALUES (?, ?, ?, datetime('now'))
        """, (agent_id, round(avg_cost or 0, 6), int(avg_tokens or 0)))

    conn.commit()


def main():
    print(f"[myelin] Starting collection at {datetime.utcnow().isoformat()}")

    conn = sqlite3.connect(DB_PATH)

    # Collect from sources
    litellm_logs = collect_litellm_spend()
    openrouter = collect_openrouter_credits()
    local = collect_local_usage()
    oauth_estimate = collect_oauth_session_estimate()

    # Parse and store LiteLLM logs
    stored_count = 0
    if isinstance(litellm_logs, list):
        for entry in litellm_logs:
            if not isinstance(entry, dict):
                continue
            # Normalize LiteLLM log entry
            model = entry.get('model', entry.get('model_id', 'unknown'))
            is_local = any(x in model.lower() for x in ['qwen', 'ollama', 'local', 'deepseek'])

            conn.execute("""
                INSERT OR IGNORE INTO usage_log
                    (timestamp, agent_id, model, provider, route,
                     input_tokens, output_tokens, cost_usd, task_context)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            """, (
                entry.get('startTime', entry.get('created_at', datetime.utcnow().isoformat())),
                entry.get('metadata', {}).get('agent_id', entry.get('user', 'unknown')),
                model,
                'local' if is_local else 'cloud',
                'ollama' if is_local else entry.get('custom_llm_provider', 'litellm'),
                entry.get('prompt_tokens', entry.get('input_tokens', 0)),
                entry.get('completion_tokens', entry.get('output_tokens', 0)),
                float(entry.get('spend', entry.get('cost', 0))) if not is_local else 0.0,
                entry.get('metadata', {}).get('task', ''),
            ))
            stored_count += 1

    conn.commit()

    # Update OpenRouter subscription state
    if 'balance' in openrouter and openrouter.get('balance') is not None:
        conn.execute("""
            INSERT OR REPLACE INTO subscriptions
                (provider, plan, current_usage, last_checked)
            VALUES ('openrouter', 'credits', ?, datetime('now'))
        """, (openrouter.get('usage', 0),))
        conn.commit()

    # Update baselines
    update_agent_baselines(conn)

    # Compute stats
    daily, per_agent = compute_daily_stats(conn)
    conn.close()

    # Write usage.json for Dashboard
    usage = {
        'updated': datetime.utcnow().isoformat(),
        'today': daily,
        'per_agent': per_agent,
        'openrouter': {**openrouter, 'measurement_type': 'measured'},  # Actual API data
        'local_models': list(local.keys()),
        'litellm_logs_imported': stored_count,
        'claude_max': {  # OAuth session — estimated, NOT measured
            **oauth_estimate,
            'measurement_type': 'estimated',
            'disclaimer': 'Claude Max usage estimated from session timing. Not from actual token counts.'
        },
        'measurement_note': 'OpenRouter/LiteLLM: measured from API. Claude Max: estimated from session timing.',
    }

    OUTPUT_PATH.write_text(json.dumps(usage, indent=2))

    print(f"[myelin] Done. Today: ${daily['cost_usd']:.4f}, {daily['tokens']:,} tokens, {stored_count} log entries imported")
    print(f"[myelin] Usage written to {OUTPUT_PATH}")
    return usage


if __name__ == '__main__':
    # Load env from axon/.env
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                v = v.strip().strip('"').strip("'")
                os.environ.setdefault(k.strip(), v)

    main()
