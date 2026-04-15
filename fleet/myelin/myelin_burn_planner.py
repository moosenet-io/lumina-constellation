#!/usr/bin/env python3
"""
myelin_burn_planner.py — Daily budget pacing and throttle suggestions (MYL P4-1)
fleet/myelin/myelin_burn_planner.py

Reads myelin.db, checks spend vs daily/weekly/monthly limits, and outputs
structured throttle suggestions. Runs at 18:00 daily via systemd timer.
Output goes to myelin.db (burn_plans table) and stdout.

No LLM — pure Python budget arithmetic.
"""
import os
import json
import sqlite3
import yaml
from datetime import datetime, timedelta, timezone
from pathlib import Path

DB_PATH = os.environ.get('MYELIN_DB_PATH', '/opt/lumina-fleet/myelin/myelin.db')
CONFIG_PATH = os.environ.get('MYELIN_CONFIG_PATH', '/opt/lumina-fleet/myelin/myelin_config.yaml')


def load_config() -> dict:
    try:
        return yaml.safe_load(Path(CONFIG_PATH).read_text()) or {}
    except Exception:
        return {}


def get_spend(db_path: str) -> dict:
    """Return spend totals for today, this week, and this month."""
    now = datetime.now(timezone.utc)
    today_start = now.replace(hour=0, minute=0, second=0, microsecond=0).isoformat()
    week_start = (now - timedelta(days=now.weekday())).replace(
        hour=0, minute=0, second=0, microsecond=0).isoformat()
    month_start = now.replace(day=1, hour=0, minute=0, second=0, microsecond=0).isoformat()

    conn = sqlite3.connect(db_path)
    cur = conn.cursor()

    def query_sum(cutoff):
        cur.execute("SELECT COALESCE(SUM(cost_usd), 0), COUNT(*) FROM usage_log WHERE timestamp >= ?",
                    (cutoff,))
        row = cur.fetchone()
        return round(row[0], 4), row[1]

    def query_by_agent(cutoff):
        cur.execute("""
            SELECT agent_id, SUM(cost_usd), COUNT(*)
            FROM usage_log WHERE timestamp >= ? AND cost_usd > 0
            GROUP BY agent_id ORDER BY SUM(cost_usd) DESC
        """, (cutoff,))
        return {(r[0] or 'unknown'): {'cost': round(r[1], 4), 'calls': r[2]}
                for r in cur.fetchall()}

    today_cost, today_calls = query_sum(today_start)
    week_cost, week_calls = query_sum(week_start)
    month_cost, month_calls = query_sum(month_start)
    agents_today = query_by_agent(today_start)

    conn.close()
    return {
        'today': {'cost': today_cost, 'calls': today_calls},
        'week': {'cost': week_cost, 'calls': week_calls},
        'month': {'cost': month_cost, 'calls': month_calls},
        'agents_today': agents_today,
    }


def compute_pacing(spend: dict, config: dict) -> dict:
    """Compute burn rates and days-remaining projections."""
    now = datetime.now(timezone.utc)
    days_in_month = 30
    day_of_month = now.day
    days_remaining_month = days_in_month - day_of_month + 1
    day_of_week = now.weekday()  # 0=Mon
    days_remaining_week = 7 - day_of_week

    budget = config.get('budget', {})
    daily_limit = budget.get('daily_soft_limit', 3.00)
    weekly_limit = budget.get('weekly_soft_limit', 15.00)
    monthly_limit = budget.get('monthly_soft_limit', 50.00)

    today_cost = spend['today']['cost']
    week_cost = spend['week']['cost']
    month_cost = spend['month']['cost']

    # Projected totals
    days_elapsed_week = max(day_of_week, 1)
    days_elapsed_month = max(day_of_month, 1)
    avg_daily_week = week_cost / days_elapsed_week
    avg_daily_month = month_cost / days_elapsed_month

    projected_week = week_cost + avg_daily_week * days_remaining_week
    projected_month = month_cost + avg_daily_month * days_remaining_month

    return {
        'daily_limit': daily_limit,
        'weekly_limit': weekly_limit,
        'monthly_limit': monthly_limit,
        'today_pct': round(today_cost / daily_limit * 100, 1) if daily_limit else 0,
        'week_pct': round(week_cost / weekly_limit * 100, 1) if weekly_limit else 0,
        'month_pct': round(month_cost / monthly_limit * 100, 1) if monthly_limit else 0,
        'projected_week': round(projected_week, 2),
        'projected_month': round(projected_month, 2),
        'projected_week_pct': round(projected_week / weekly_limit * 100, 1) if weekly_limit else 0,
        'projected_month_pct': round(projected_month / monthly_limit * 100, 1) if monthly_limit else 0,
        'avg_daily_week': round(avg_daily_week, 4),
        'days_remaining_week': days_remaining_week,
        'days_remaining_month': days_remaining_month,
    }


def build_suggestions(spend: dict, pacing: dict, config: dict) -> list[dict]:
    """Generate throttle suggestions based on pacing data."""
    suggestions = []
    threshold = config.get('alerts', {}).get('burn_suggestion_threshold', 30)
    agent_budgets = config.get('agent_budgets', {})

    # Daily overage
    if pacing['today_pct'] >= 100:
        suggestions.append({
            'level': 'critical',
            'scope': 'daily',
            'message': f"Daily limit exceeded: ${spend['today']['cost']:.2f} vs ${pacing['daily_limit']:.2f} limit",
            'action': 'pause_non_essential',
        })
    elif pacing['today_pct'] >= 80:
        suggestions.append({
            'level': 'warn',
            'scope': 'daily',
            'message': f"Daily spend at {pacing['today_pct']:.0f}% of limit (${spend['today']['cost']:.2f})",
            'action': 'reduce_frequency',
        })

    # Weekly projection
    if pacing['projected_week_pct'] >= 120:
        suggestions.append({
            'level': 'critical',
            'scope': 'weekly',
            'message': f"Weekly spend projected at ${pacing['projected_week']:.2f} ({pacing['projected_week_pct']:.0f}% of ${pacing['weekly_limit']:.2f} limit)",
            'action': 'throttle_seer_research',
        })
    elif pacing['projected_week_pct'] >= 100:
        suggestions.append({
            'level': 'warn',
            'scope': 'weekly',
            'message': f"Weekly spend on pace to exceed limit: ${pacing['projected_week']:.2f} projected vs ${pacing['weekly_limit']:.2f}",
            'action': 'defer_non_urgent_tasks',
        })

    # Monthly projection
    if pacing['projected_month_pct'] >= 100:
        remaining_budget = pacing['monthly_limit'] - spend['month']['cost']
        if remaining_budget > 0 and pacing['days_remaining_month'] > 0:
            daily_allowance = remaining_budget / pacing['days_remaining_month']
            suggestions.append({
                'level': 'warn',
                'scope': 'monthly',
                'message': f"Monthly spend on pace to exceed limit. Allowance: ${daily_allowance:.2f}/day for remaining {pacing['days_remaining_month']} days",
                'action': 'apply_daily_allowance',
                'daily_allowance': round(daily_allowance, 2),
            })

    # Per-agent overspend
    for agent, data in spend['agents_today'].items():
        if agent in agent_budgets:
            limit_key = 'per_report_limit' if agent == 'seer' else \
                        'per_session_limit' if agent == 'wizard' else \
                        'per_loop_limit'
            limit = agent_budgets[agent].get(limit_key, 0)
            if limit and data['cost'] > limit:
                suggestions.append({
                    'level': 'warn',
                    'scope': f'agent:{agent}',
                    'message': f"{agent} spent ${data['cost']:.4f} today vs ${limit:.2f} per-task limit",
                    'action': f'review_{agent}_usage',
                })

    # Positive — all OK
    if not suggestions:
        suggestions.append({
            'level': 'ok',
            'scope': 'all',
            'message': f"Burn rate nominal. Today: ${spend['today']['cost']:.4f} ({pacing['today_pct']:.0f}% of daily limit). Avg/day this week: ${pacing['avg_daily_week']:.4f}",
            'action': 'none',
        })

    return suggestions


def ensure_burn_plans_table(db_path: str):
    conn = sqlite3.connect(db_path)
    conn.execute("""
        CREATE TABLE IF NOT EXISTS burn_plans (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            spend_json TEXT NOT NULL,
            pacing_json TEXT NOT NULL,
            suggestions_json TEXT NOT NULL
        )
    """)
    conn.commit()
    conn.close()


def save_plan(db_path: str, spend: dict, pacing: dict, suggestions: list):
    conn = sqlite3.connect(db_path)
    conn.execute(
        "INSERT INTO burn_plans (spend_json, pacing_json, suggestions_json) VALUES (?, ?, ?)",
        (json.dumps(spend), json.dumps(pacing), json.dumps(suggestions))
    )
    # Keep last 90 days of plans
    conn.execute("""
        DELETE FROM burn_plans WHERE timestamp < datetime('now', '-90 days')
    """)
    conn.commit()
    conn.close()


def main():
    import argparse
    parser = argparse.ArgumentParser(description='Myelin burn planner — daily budget pacing')
    parser.add_argument('--json', action='store_true', help='Output JSON instead of text')
    parser.add_argument('--no-save', action='store_true', help='Skip writing to DB')
    args = parser.parse_args()

    config = load_config()
    spend = get_spend(DB_PATH)
    pacing = compute_pacing(spend, config)
    suggestions = build_suggestions(spend, pacing, config)

    if not args.no_save:
        ensure_burn_plans_table(DB_PATH)
        save_plan(DB_PATH, spend, pacing, suggestions)

    if args.json:
        print(json.dumps({
            'spend': spend,
            'pacing': pacing,
            'suggestions': suggestions,
            'generated_at': datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M UTC'),
        }, indent=2))
        return

    # Human-readable output
    now_str = datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M UTC')
    print(f"[myelin-burn-planner] {now_str}")
    print(f"  Today:   ${spend['today']['cost']:.4f} / ${pacing['daily_limit']:.2f}  ({pacing['today_pct']:.0f}%)")
    print(f"  Week:    ${spend['week']['cost']:.4f} / ${pacing['weekly_limit']:.2f}  ({pacing['week_pct']:.0f}%)  → projected ${pacing['projected_week']:.2f}")
    print(f"  Month:   ${spend['month']['cost']:.4f} / ${pacing['monthly_limit']:.2f}  ({pacing['month_pct']:.0f}%)  → projected ${pacing['projected_month']:.2f}")
    print()
    for s in suggestions:
        icon = {'critical': '🔴', 'warn': '🟡', 'ok': '🟢'}.get(s['level'], '⚪')
        print(f"  {icon} [{s['scope']}] {s['message']}")
        if s['action'] != 'none':
            print(f"      → {s['action']}")


if __name__ == '__main__':
    main()
