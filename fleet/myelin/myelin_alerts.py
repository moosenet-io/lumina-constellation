#!/usr/bin/env python3
"""
Myelin alert engine — runs every 30 min after collection.
Checks subscription limits, runaway agents, efficiency issues.
Sends alerts via Nexus inbox (psycopg2 to postgres-host PostgreSQL).
"""
import os, sys, json, sqlite3, urllib.request
from datetime import datetime, timedelta
from pathlib import Path

sys.path.insert(0, '/opt/lumina-fleet')
try: from naming import display_name as _dn
except: _dn = lambda x: x

DB_PATH = os.environ.get('MYELIN_DB_PATH', '/opt/lumina-fleet/myelin/myelin.db')
CONFIG_PATH = '/opt/lumina-fleet/myelin/myelin_config.yaml'
MATRIX_URL = os.environ.get('MATRIX_BRIDGE_HOMESERVER', '')
INBOX_DB_HOST = os.environ.get('INBOX_DB_HOST', '')
INBOX_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', '')


def load_config():
    try:
        import yaml
        with open(CONFIG_PATH) as f:
            return yaml.safe_load(f)
    except:
        return {
            'alerts': {'subscription_warn_percent': 80, 'subscription_urgent_percent': 95,
                       'runaway_threshold_multiplier': 3},
            'budget': {'daily_soft_limit': 3.0}
        }


def send_alert(title: str, body: str, priority: str = 'normal'):
    """Send alert to Lumina via Nexus inbox."""
    payload = {'title': title, 'body': body, 'source': 'myelin'}

    # Try Nexus DB first
    if INBOX_DB_HOST:
        try:
            import psycopg2
            conn = psycopg2.connect(host=INBOX_DB_HOST, dbname='lumina_inbox',
                                    user=INBOX_DB_USER, password=INBOX_DB_PASS, connect_timeout=5)
            cur = conn.cursor()
            cur.execute("""
                INSERT INTO inbox_messages (from_agent, to_agent, message_type, payload, priority, status)
                VALUES ('myelin', 'lumina', 'alert', %s, %s, 'pending')
            """, (json.dumps(payload), priority))
            conn.commit(); conn.close()
            print(f"[myelin-alerts] Alert sent via Nexus ({priority}): {title}")
            return True
        except Exception as e:
            print(f"[myelin-alerts] Nexus send failed: {e}")

    # Fallback: write to log file
    alert_log = Path('/opt/lumina-fleet/myelin/logs/alerts.log')
    alert_log.parent.mkdir(parents=True, exist_ok=True)
    with open(alert_log, 'a') as f:
        f.write(json.dumps({'ts': datetime.utcnow().isoformat(), 'priority': priority,
                            'title': title, 'body': body}) + '\n')
    print(f"[myelin-alerts] Alert logged ({priority}): {title}")
    return False


def check_subscription_limits(conn, config):
    """Check subscription usage against configured thresholds."""
    alerts_fired = []
    warn_pct = config.get('alerts', {}).get('subscription_warn_percent', 80)
    urgent_pct = config.get('alerts', {}).get('subscription_urgent_percent', 95)

    cur = conn.cursor()
    cur.execute("SELECT provider, plan, current_usage, limit_value, limit_unit, cycle_end FROM subscriptions")

    for row in cur.fetchall():
        provider, plan, usage, limit, unit, cycle_end = row

        if not limit or limit == 0:
            continue

        if unit in ('percent', 'usd'):
            pct_used = (usage / limit) * 100 if unit == 'percent' else 0
        else:
            continue

        if pct_used >= urgent_pct:
            send_alert(
                f"🔴 {provider.title()} subscription at {pct_used:.0f}%",
                f"Plan: {plan}. Usage: {usage:.2f}/{limit:.2f} {unit}. " +
                (f"Cycle ends: {cycle_end}" if cycle_end else ""),
                priority='urgent'
            )
            alerts_fired.append(f"{provider}: urgent ({pct_used:.0f}%)")
        elif pct_used >= warn_pct:
            send_alert(
                f"⚠️ {provider.title()} subscription at {pct_used:.0f}%",
                f"Plan: {plan}. Usage: {usage:.2f}/{limit:.2f} {unit}. " +
                (f"Cycle ends: {cycle_end}" if cycle_end else ""),
                priority='normal'
            )
            alerts_fired.append(f"{provider}: warn ({pct_used:.0f}%)")

    return alerts_fired


def check_runaway_agents(conn, config):
    """Detect agents spending 3x+ their normal daily cost."""
    alerts_fired = []
    threshold = config.get('alerts', {}).get('runaway_threshold_multiplier', 3)
    today = datetime.utcnow().date().isoformat()

    cur = conn.cursor()
    cur.execute("""
        SELECT u.agent_id, SUM(u.cost_usd) as today_cost,
               b.avg_daily_cost
        FROM (
            SELECT agent_id, cost_usd FROM usage_log
            WHERE timestamp >= ? AND cost_usd > 0
        ) u
        LEFT JOIN agent_baselines b ON b.agent_id = u.agent_id
        GROUP BY u.agent_id
        HAVING today_cost > 0.01
    """, (f"{today}T00:00:00",))

    for agent_id, today_cost, avg_cost in cur.fetchall():
        if avg_cost and avg_cost > 0:
            ratio = today_cost / avg_cost
            if ratio >= threshold:
                send_alert(
                    f"🚨 Runaway agent: {_dn(agent_id)} at {ratio:.1f}x normal cost",
                    f"Today's spend: ${today_cost:.3f}. Normal daily: ${avg_cost:.3f}. "
                    f"This is {ratio:.1f}x typical. Consider checking for infinite loops.",
                    priority='urgent'
                )
                alerts_fired.append(f"{agent_id}: {ratio:.1f}x")

    return alerts_fired


def check_daily_budget(conn, config):
    """Check if today's total spend exceeds soft limit."""
    today = datetime.utcnow().date().isoformat()
    daily_limit = config.get('budget', {}).get('daily_soft_limit', 3.0)

    cur = conn.cursor()
    cur.execute("""
        SELECT COALESCE(SUM(cost_usd), 0) FROM usage_log
        WHERE timestamp >= ?
    """, (f"{today}T00:00:00",))

    today_cost = cur.fetchone()[0]

    if today_cost > daily_limit:
        send_alert(
            f"💰 Daily spend limit reached: ${today_cost:.2f}",
            f"Today's total inference cost (${today_cost:.2f}) exceeds your soft limit (${daily_limit:.2f}). "
            f"This is advisory only — no agents have been stopped.",
            priority='normal'
        )
        return True
    return False


def main():
    print(f"[myelin-alerts] Starting alert check at {datetime.utcnow().isoformat()}")

    # Load env
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                v = v.strip().strip('"').strip("'")
                os.environ.setdefault(k.strip(), v)

    config = load_config()
    conn = sqlite3.connect(DB_PATH)

    sub_alerts = check_subscription_limits(conn, config)
    runaway_alerts = check_runaway_agents(conn, config)
    budget_hit = check_daily_budget(conn, config)

    conn.close()

    total = len(sub_alerts) + len(runaway_alerts) + (1 if budget_hit else 0)
    print(f"[myelin-alerts] Done. {total} alerts fired.")
    if sub_alerts: print(f"  Subscription: {sub_alerts}")
    if runaway_alerts: print(f"  Runaway: {runaway_alerts}")
    if budget_hit: print(f"  Budget: daily limit exceeded")


if __name__ == '__main__':
    main()
