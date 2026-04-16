#!/usr/bin/env python3
"""
myelin_weekly_report.py — Weekly HTML cost report for Lumina Constellation (MYL P4-2)
fleet/myelin/myelin_weekly_report.py

Reads myelin.db, generates a 7-day cost summary HTML report,
writes to /var/www/html/cost/ (served by Apache on fleet host at /cost/).

Usage:
  python3 myelin_weekly_report.py [--output /path/to/report.html] [--days 7]
"""
import os
import sys
import json
import sqlite3
from datetime import datetime, timedelta, timezone
from pathlib import Path

DB_PATH = os.environ.get('MYELIN_DB_PATH', '/opt/lumina-fleet/myelin/myelin.db')
OUTPUT_DIR = os.environ.get('MYELIN_REPORT_DIR', '/var/www/html/cost')
REPORT_FILE = 'index.html'


def load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


def get_weekly_data(db_path: str, days: int = 7) -> dict:
    """Read cost data from myelin.db for the last N days."""
    cutoff = (datetime.now(timezone.utc) - timedelta(days=days)).isoformat()

    try:
        conn = sqlite3.connect(db_path)
        cur = conn.cursor()

        # Daily totals
        cur.execute("""
            SELECT DATE(timestamp) as day, SUM(cost_usd) as total, COUNT(*) as calls
            FROM usage_log
            WHERE timestamp >= ? AND cost_usd > 0
            GROUP BY DATE(timestamp)
            ORDER BY day DESC
        """, (cutoff,))
        daily = [{'day': r[0], 'total': round(r[1], 4), 'calls': r[2]} for r in cur.fetchall()]

        # Per-agent totals
        cur.execute("""
            SELECT agent_id, SUM(cost_usd) as total, COUNT(*) as calls,
                   SUM(prompt_tokens + completion_tokens) as tokens
            FROM usage_log
            WHERE timestamp >= ? AND cost_usd > 0
            GROUP BY agent_id
            ORDER BY total DESC
        """, (cutoff,))
        by_agent = [{'agent': r[0] or 'unknown', 'total': round(r[1], 4),
                     'calls': r[2], 'tokens': r[3] or 0} for r in cur.fetchall()]

        # Per-model totals
        cur.execute("""
            SELECT model, SUM(cost_usd) as total, COUNT(*) as calls
            FROM usage_log
            WHERE timestamp >= ? AND cost_usd > 0
            GROUP BY model
            ORDER BY total DESC
            LIMIT 10
        """, (cutoff,))
        by_model = [{'model': r[0] or 'unknown', 'total': round(r[1], 4), 'calls': r[2]}
                    for r in cur.fetchall()]

        # Grand total
        cur.execute("SELECT COALESCE(SUM(cost_usd), 0), COUNT(*) FROM usage_log WHERE timestamp >= ?",
                    (cutoff,))
        grand = cur.fetchone()
        grand_total = round(grand[0], 4)
        grand_calls = grand[1]

        # Today
        today_cutoff = datetime.now(timezone.utc).date().isoformat() + 'T00:00:00'
        cur.execute("SELECT COALESCE(SUM(cost_usd), 0), COUNT(*) FROM usage_log WHERE timestamp >= ?",
                    (today_cutoff,))
        today = cur.fetchone()
        today_total = round(today[0], 4)
        today_calls = today[1]

        conn.close()
        return {
            'daily': daily,
            'by_agent': by_agent,
            'by_model': by_model,
            'grand_total': grand_total,
            'grand_calls': grand_calls,
            'today_total': today_total,
            'today_calls': today_calls,
            'days': days,
            'generated_at': datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M UTC'),
        }
    except Exception as e:
        return {'error': str(e), 'daily': [], 'by_agent': [], 'by_model': [],
                'grand_total': 0, 'grand_calls': 0, 'today_total': 0, 'today_calls': 0,
                'days': days, 'generated_at': datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}


def status_badge(cost: float, warn: float = 2.0, crit: float = 10.0) -> str:
    if cost >= crit:
        return '<span style="color:#f85149;font-weight:700;">HIGH</span>'
    if cost >= warn:
        return '<span style="color:#d29922;font-weight:700;">WARN</span>'
    return '<span style="color:#3fb950;font-weight:700;">OK</span>'


def bar_chart(agents: list, total: float) -> str:
    if not agents or total == 0:
        return '<p style="color:var(--text-tertiary);">No data</p>'
    colors = ['#58a6ff', '#3fb950', '#d29922', '#f85149', '#bc8cff',
              '#79c0ff', '#56d364', '#e3b341', '#ff7b72', '#d2a8ff']
    bars = []
    for i, a in enumerate(agents[:8]):
        pct = (a['total'] / total * 100) if total > 0 else 0
        color = colors[i % len(colors)]
        bars.append(
            f'<div style="margin-bottom:0.5rem;">'
            f'<div style="display:flex;justify-content:space-between;font-size:0.8rem;margin-bottom:2px;">'
            f'<span style="color:var(--text-secondary);">{a["agent"]}</span>'
            f'<span style="color:var(--text-primary);">${a["total"]:.4f} ({pct:.1f}%)</span>'
            f'</div>'
            f'<div style="background:var(--bg-tertiary);border-radius:3px;height:8px;">'
            f'<div style="background:{color};width:{min(pct,100):.1f}%;height:100%;border-radius:3px;"></div>'
            f'</div></div>'
        )
    return ''.join(bars)


def generate_html(data: dict) -> str:
    daily_rows = ''
    for d in data['daily']:
        daily_rows += (
            f'<tr><td>{d["day"]}</td>'
            f'<td style="text-align:right;">${d["total"]:.4f}</td>'
            f'<td style="text-align:right;">{d["calls"]}</td>'
            f'<td>{status_badge(d["total"])}</td></tr>'
        )
    if not daily_rows:
        daily_rows = '<tr><td colspan="4" style="color:var(--text-tertiary);text-align:center;">No data</td></tr>'

    agent_rows = ''
    for a in data['by_agent']:
        agent_rows += (
            f'<tr><td>{a["agent"]}</td>'
            f'<td style="text-align:right;">${a["total"]:.4f}</td>'
            f'<td style="text-align:right;">{a["calls"]}</td>'
            f'<td style="text-align:right;">{a["tokens"]:,}</td></tr>'
        )
    if not agent_rows:
        agent_rows = '<tr><td colspan="4" style="color:var(--text-tertiary);text-align:center;">No data</td></tr>'

    model_rows = ''
    for m in data['by_model']:
        model_rows += (
            f'<tr><td style="font-size:0.8rem;">{m["model"]}</td>'
            f'<td style="text-align:right;">${m["total"]:.4f}</td>'
            f'<td style="text-align:right;">{m["calls"]}</td></tr>'
        )
    if not model_rows:
        model_rows = '<tr><td colspan="3" style="color:var(--text-tertiary);text-align:center;">No data</td></tr>'

    chart = bar_chart(data['by_agent'], data['grand_total'])
    err_banner = f'<div style="background:#f85149;color:#fff;padding:0.5rem 1rem;border-radius:6px;margin-bottom:1rem;">Error reading DB: {data["error"]}</div>' if 'error' in data else ''

    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Lumina Cost Report — {data['days']}-Day Summary</title>
<link rel="stylesheet" href="/shared/constellation.css">
<style>
  body {{ padding: 2rem; }}
  .metric-grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 1rem; margin-bottom: 1.5rem; }}
  .metric {{ text-align: center; }}
  .metric .value {{ font-size: 2rem; font-weight: 700; color: var(--accent); }}
  .metric .label {{ font-size: 0.8rem; color: var(--text-secondary); margin-top: 0.25rem; }}
  table {{ width: 100%; border-collapse: collapse; font-size: 0.85rem; }}
  th {{ text-align: left; padding: 0.4rem 0.6rem; border-bottom: 1px solid var(--border-color);
        color: var(--text-secondary); font-weight: 600; font-size: 0.75rem; text-transform: uppercase; }}
  td {{ padding: 0.4rem 0.6rem; border-bottom: 1px solid var(--bg-tertiary); }}
  tr:last-child td {{ border-bottom: none; }}
  h2 {{ font-size: 1rem; font-weight: 700; margin: 0 0 1rem 0; }}
  .section {{ margin-bottom: 1.5rem; }}
</style>
</head>
<body>
<div style="max-width:900px;margin:0 auto;">
  <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:1.5rem;">
    <h1 style="font-size:1.5rem;font-weight:700;margin:0;">&#x1F4A1; Lumina Cost Report</h1>
    <div style="font-size:0.8rem;color:var(--text-tertiary);">{data['days']}-day window · Generated {data['generated_at']}</div>
  </div>

  {err_banner}

  <div class="metric-grid">
    <div class="card metric">
      <div class="value">${data['grand_total']:.2f}</div>
      <div class="label">{data['days']}-day total {status_badge(data['grand_total'], 10, 50)}</div>
    </div>
    <div class="card metric">
      <div class="value">${data['today_total']:.2f}</div>
      <div class="label">today {status_badge(data['today_total'])}</div>
    </div>
    <div class="card metric">
      <div class="value">{data['grand_calls']:,}</div>
      <div class="label">{data['days']}-day API calls</div>
    </div>
    <div class="card metric">
      <div class="value">${data['grand_total']/data['days']:.2f}</div>
      <div class="label">avg per day</div>
    </div>
  </div>

  <div style="display:grid;grid-template-columns:1fr 1fr;gap:1.5rem;margin-bottom:1.5rem;">
    <div class="card section">
      <h2>Daily Breakdown</h2>
      <table>
        <thead><tr><th>Date</th><th style="text-align:right;">Cost</th><th style="text-align:right;">Calls</th><th>Status</th></tr></thead>
        <tbody>{daily_rows}</tbody>
      </table>
    </div>
    <div class="card section">
      <h2>Cost by Agent</h2>
      {chart}
    </div>
  </div>

  <div style="display:grid;grid-template-columns:1fr 1fr;gap:1.5rem;">
    <div class="card section">
      <h2>Per-Agent Detail</h2>
      <table>
        <thead><tr><th>Agent</th><th style="text-align:right;">Cost</th><th style="text-align:right;">Calls</th><th style="text-align:right;">Tokens</th></tr></thead>
        <tbody>{agent_rows}</tbody>
      </table>
    </div>
    <div class="card section">
      <h2>Top Models</h2>
      <table>
        <thead><tr><th>Model</th><th style="text-align:right;">Cost</th><th style="text-align:right;">Calls</th></tr></thead>
        <tbody>{model_rows}</tbody>
      </table>
    </div>
  </div>

  <div style="margin-top:1.5rem;font-size:0.75rem;color:var(--text-tertiary);text-align:center;">
    Lumina Constellation · Myelin Cost Reporting · <a href="http://localhost:8082/status" style="color:var(--accent);">Soma Dashboard</a>
  </div>
</div>
</body>
</html>"""


def main():
    import argparse
    parser = argparse.ArgumentParser(description='Generate Myelin weekly cost report')
    parser.add_argument('--output', default=None, help='Output file path')
    parser.add_argument('--days', type=int, default=7, help='Days to include (default 7)')
    parser.add_argument('--stdout', action='store_true', help='Print to stdout instead of file')
    args = parser.parse_args()

    load_env()
    data = get_weekly_data(DB_PATH, args.days)
    html = generate_html(data)

    if args.stdout:
        print(html)
        return

    output_path = args.output or os.path.join(OUTPUT_DIR, REPORT_FILE)
    Path(output_path).parent.mkdir(parents=True, exist_ok=True)
    Path(output_path).write_text(html)
    print(f'[myelin-report] Written to {output_path} ({len(html):,} bytes, ${data["grand_total"]:.4f} over {args.days} days)')


if __name__ == '__main__':
    main()
