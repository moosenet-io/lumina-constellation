import subprocess, json, os

# ============================================================
# Myelin Tools — Token Governance and Cost Intelligence
# terminus-host SSHes to fleet-host to query myelin.py
# ============================================================

MYELIN_HOST = 'root@YOUR_FLEET_SERVER_IP'
MYELIN_DIR = '/opt/lumina-fleet/myelin'


def _myelin(cmd, timeout=20):
    """Run a command on fleet-host in the myelin directory."""
    env_setup = 'source /opt/lumina-fleet/axon/.env && export LITELLM_MASTER_KEY INBOX_DB_HOST INBOX_DB_USER INBOX_DB_PASS OPENROUTER_API_KEY'
    full = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {MYELIN_HOST} '{env_setup} && {cmd}'"
    try:
        r = subprocess.run(full, shell=True, capture_output=True, text=True, timeout=timeout)
        if r.returncode != 0:
            return {'error': r.stderr.strip()[:200] or r.stdout.strip()[:200]}
        try:
            return json.loads(r.stdout.strip())
        except:
            return {'output': r.stdout.strip()[:500]}
    except Exception as e:
        return {'error': str(e)}


def _db_query(query, params=()):
    """Query myelin.db on fleet-host directly."""
    safe_query = query.replace("'", "\\'")
    cmd = f"python3 -c \"import sqlite3,json; conn=sqlite3.connect('/opt/lumina-fleet/myelin/myelin.db'); cur=conn.cursor(); cur.execute('{safe_query}'); rows=cur.fetchall(); print(json.dumps(rows))\""
    return _myelin(cmd)


def register_myelin_tools(mcp):

    @mcp.tool()
    def myelin_status() -> str:
        """Myelin token governance — today's cost summary. Returns formatted string, no LLM needed."""
        cmd = "cat /opt/lumina-fleet/myelin/output/usage.json 2>/dev/null || echo '{\"error\": \"no data\"}'"
        result = _myelin(cmd)
        if not isinstance(result, dict) or 'error' in result:
            return "Myelin: no data yet (collecting every 30min)."
        today = result.get('today', {})
        cost = today.get('cost_usd', 0)
        tokens = today.get('total_tokens', 0)
        calls = today.get('calls', 0)
        per_agent = result.get('per_agent', {})
        top = sorted(per_agent.items(), key=lambda x: x[1].get('cost', 0), reverse=True)[:3]
        agents = ', '.join(f'{a}: ${d.get("cost",0):.3f}' for a,d in top) or 'no data'
        or_bal = result.get('openrouter', {}).get('balance')
        or_str = f' | OpenRouter: ${or_bal:.2f} remaining' if or_bal else ''
        return (f"Today: ${cost:.4f}, {tokens:,} tokens, {calls} calls. Agents: {agents}.{or_str}")

    @mcp.tool()
    def myelin_today() -> dict:
        """Get today's inference cost breakdown by agent and model.
        Returns: {total_cost, per_agent: {agent: {cost, tokens}}, top_models}"""
        from datetime import datetime, date
        today = date.today().isoformat()
        cmd = f"""python3 -c "
import sqlite3, json
from datetime import datetime, date
conn = sqlite3.connect('/opt/lumina-fleet/myelin/myelin.db')
cur = conn.cursor()
cur.execute('SELECT COALESCE(SUM(cost_usd),0), COALESCE(SUM(input_tokens+output_tokens),0), COUNT(*) FROM usage_log WHERE timestamp >= \\\"$(date +%Y-%m-%d)T00:00:00\\\"')
row = cur.fetchone()
cur.execute('SELECT agent_id, COALESCE(SUM(cost_usd),0), COALESCE(SUM(input_tokens+output_tokens),0) FROM usage_log WHERE timestamp >= \\\"$(date +%Y-%m-%d)T00:00:00\\\" GROUP BY agent_id ORDER BY 2 DESC')
agents = {{r[0]: {{\"cost\": round(r[1],4), \"tokens\": r[2]}} for r in cur.fetchall()}}
print(json.dumps({{\"date\": str(date.today()), \"total_cost\": round(row[0],4), \"total_tokens\": row[1], \"calls\": row[2], \"per_agent\": agents}}))
conn.close()
" """
        return _myelin(cmd)

    @mcp.tool()
    def myelin_agent_cost(agent_id: str, days_back: int = 7) -> dict:
        """Get cost breakdown for a specific agent over N days.
        agent_id: lumina, seer, wizard, vector, axon, vigil, sentinel, etc.
        Returns: {daily_average, total, by_model, trend}"""
        cmd = f"""python3 -c "
import sqlite3, json
from datetime import datetime, timedelta
conn = sqlite3.connect('/opt/lumina-fleet/myelin/myelin.db')
cur = conn.cursor()
cutoff = (datetime.utcnow() - timedelta(days={days_back})).isoformat()
cur.execute('SELECT DATE(timestamp), COALESCE(SUM(cost_usd),0), COALESCE(SUM(input_tokens+output_tokens),0) FROM usage_log WHERE agent_id=? AND timestamp>=? GROUP BY DATE(timestamp)', ('{agent_id}', cutoff))
daily = {{r[0]: {{\"cost\": round(r[1],4), \"tokens\": r[2]}} for r in cur.fetchall()}}
total = sum(v[\"cost\"] for v in daily.values())
avg = total / max(len(daily), 1)
cur.execute('SELECT model, COALESCE(SUM(cost_usd),0) FROM usage_log WHERE agent_id=? AND timestamp>=? GROUP BY model ORDER BY 2 DESC LIMIT 5', ('{agent_id}', cutoff))
models = {{r[0]: round(r[1],4) for r in cur.fetchall()}}
print(json.dumps({{\"agent_id\": \"{agent_id}\", \"days\": {days_back}, \"total\": round(total,4), \"daily_average\": round(avg,4), \"by_day\": daily, \"by_model\": models}}))
conn.close()
" """
        return _myelin(cmd)

    @mcp.tool()
    def myelin_subscription_status() -> dict:
        """Get current subscription usage for all providers (Anthropic, OpenRouter, Ollama).
        Returns: {provider: {plan, usage, limit, percent_used, days_until_reset}}"""
        cmd = "python3 -c \"import sqlite3,json; conn=sqlite3.connect('/opt/lumina-fleet/myelin/myelin.db'); cur=conn.cursor(); cur.execute('SELECT * FROM subscriptions'); rows=[{'provider':r[0],'plan':r[1],'cycle_end':r[3],'limit':r[4],'unit':r[5],'usage':r[6],'last_checked':r[7]} for r in cur.fetchall()]; print(json.dumps(rows)); conn.close()\""
        return _myelin(cmd)

    @mcp.tool()
    def myelin_weekly() -> dict:
        """Get this week's cost summary — total spend, top agents, model distribution.
        Use for weekly review or when asked about overall inference costs."""
        cmd = """python3 -c "
import sqlite3, json
from datetime import datetime, timedelta
conn = sqlite3.connect('/opt/lumina-fleet/myelin/myelin.db')
cur = conn.cursor()
cutoff = (datetime.utcnow() - timedelta(days=7)).isoformat()
cur.execute('SELECT COALESCE(SUM(cost_usd),0), COALESCE(SUM(input_tokens+output_tokens),0), COUNT(*) FROM usage_log WHERE timestamp>=?', (cutoff,))
row = cur.fetchone()
cur.execute('SELECT agent_id, COALESCE(SUM(cost_usd),0) FROM usage_log WHERE timestamp>=? GROUP BY agent_id ORDER BY 2 DESC LIMIT 8', (cutoff,))
agents = {r[0]: round(r[1],4) for r in cur.fetchall()}
cur.execute('SELECT model, COALESCE(SUM(cost_usd),0) FROM usage_log WHERE timestamp>=? GROUP BY model ORDER BY 2 DESC LIMIT 5', (cutoff,))
models = {r[0]: round(r[1],4) for r in cur.fetchall()}
print(json.dumps({'period': '7d', 'total_cost': round(row[0],4), 'total_tokens': row[1], 'total_calls': row[2], 'per_agent': agents, 'top_models': models}))
conn.close()
" """
        return _myelin(cmd)

    @mcp.tool()
    def myelin_runaway_check() -> dict:
        """Check all agents for runaway spend patterns (3x+ baseline).
        Returns list of agents exceeding their baseline with details."""
        cmd = """python3 -c "
import sqlite3, json
from datetime import datetime
conn = sqlite3.connect('/opt/lumina-fleet/myelin/myelin.db')
cur = conn.cursor()
today = datetime.utcnow().date().isoformat()
cur.execute('''SELECT u.agent_id, SUM(u.cost_usd) as today_cost, b.avg_daily_cost
    FROM (SELECT agent_id, cost_usd FROM usage_log WHERE timestamp>=? AND cost_usd>0) u
    LEFT JOIN agent_baselines b ON b.agent_id=u.agent_id
    GROUP BY u.agent_id HAVING today_cost>0.01''', (today+'T00:00:00',))
results = []
for agent_id, today_cost, avg_cost in cur.fetchall():
    ratio = (today_cost/avg_cost) if avg_cost and avg_cost>0 else 0
    results.append({'agent_id': agent_id, 'today_cost': round(today_cost,4), 'baseline': round(avg_cost or 0,4), 'ratio': round(ratio,1), 'is_runaway': ratio>=3})
print(json.dumps({'checked_at': datetime.utcnow().isoformat(), 'agents': results, 'runaways': [r['agent_id'] for r in results if r.get('is_runaway')]}))
conn.close()
" """
        return _myelin(cmd)

    @mcp.tool()
    def myelin_suggest_throttle(agent_id: str, reason: str) -> dict:
        """Suggest throttling an agent due to cost concerns. Sends proposal to Lumina via Nexus.
        agent_id: the agent to potentially throttle.
        reason: why throttling is suggested (e.g. '5x over baseline').
        Returns: {sent: bool, message: 'Proposal sent to Lumina for approval'}
        Note: Never throttles directly — always routes through Lumina for operator approval."""
        from datetime import datetime
        cmd = f"""python3 -c "
import sys, os, json
sys.path.insert(0, '/opt/lumina-fleet/nexus')
from household_routing import send_household
result = send_household('myelin', 'lumina', 'throttle_suggestion', {{
    'target_agent': '{agent_id}',
    'reason': '{reason}',
    'requires_approval': True,
    'timestamp': '{datetime.utcnow().isoformat()}'
}}, priority='normal')
print(json.dumps({{'sent': True, 'message': 'Throttle suggestion sent to Lumina for approval'}}))
" """
        return _myelin(cmd)

    @mcp.tool()
    def myelin_burn_plan() -> dict:
        """Generate a subscription burn plan — what work to run to use remaining capacity.
        Queries Engram for deferred work, estimates cost per task type.
        Returns suggested tasks to run before subscription cycle ends."""
        # Simple version: query Engram for low-priority work
        cmd = """python3 -c "
import sqlite3, json
conn = sqlite3.connect('/opt/lumina-fleet/myelin/myelin.db')
cur = conn.cursor()
cur.execute('SELECT provider, current_usage, limit_value, limit_unit, cycle_end FROM subscriptions')
subs = [{'provider': r[0], 'usage': r[1], 'limit': r[2], 'unit': r[3], 'cycle_end': r[4]} for r in cur.fetchall()]
conn.close()
# Estimate what could be done with remaining capacity
suggestions = []
for s in subs:
    if s.get('unit') == 'usd' and s.get('limit'):
        remaining = (s['limit'] or 0) - (s['usage'] or 0)
        if remaining > 2.0:
            suggestions.append({'provider': s['provider'], 'remaining': round(remaining, 2),
                'could_run': [
                    {'task': 'Obsidian Circle consultation', 'est_cost': 0.50},
                    {'task': 'Seer deep research report', 'est_cost': 3.00},
                    {'task': 'Vector dev loop iteration', 'est_cost': 0.20}
                ]})
print(json.dumps({'burn_plan': suggestions, 'note': 'Check Engram for deferred tasks to fill remaining capacity'}))
" """
        return _myelin(cmd)

    @mcp.tool()
    def myelin_set_baseline(agent_id: str, avg_daily_cost: float) -> dict:
        """Manually set the cost baseline for an agent.
        Use to establish baselines for new agents or override calculated values.
        agent_id: the agent. avg_daily_cost: expected average $ per day."""
        cmd = f"""python3 -c "
import sqlite3, json
from datetime import datetime
conn = sqlite3.connect('/opt/lumina-fleet/myelin/myelin.db')
conn.execute('INSERT OR REPLACE INTO agent_baselines (agent_id, avg_daily_cost, updated) VALUES (?, ?, datetime(\\'now\\'))', ('{agent_id}', {avg_daily_cost}))
conn.commit(); conn.close()
print(json.dumps({{'set': True, 'agent_id': '{agent_id}', 'avg_daily_cost': {avg_daily_cost}}}))
" """
        return _myelin(cmd)
