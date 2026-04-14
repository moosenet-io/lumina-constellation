"""
Vigil dashboard generator.
Generates a self-hosted HTML briefing page from section data.
Called by briefing.py after gathering all section data.
"""

import html
import json
from datetime import datetime
from pathlib import Path

OUTPUT_DIR = Path('/opt/lumina-fleet/vigil/output/html')
OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

ICONS = {
    'calendar': '📅',
    'weather': '🌤',
    'commute': '🚗',
    'news': '📰',
    'sports': '⚽',
    'crypto': '💰',
    'stocks': '📈',
    'today_tasks': '📋',
    'system_status': '🖥',
    'inbox': '📬',
    'greeting': '👋',
    'outfit': '👔',
    'fun_fact': '💡',
    'seti': '🔭',
    'reflection': '🧠',
    'cluster': '🔧',
    'plex': '🎬',
    'jellyseerr': '🎭',
    'tech_news': '🤖',
    'business_news': '💼',
    'general_news': '📰',
    'heat_warning': '🌡',
    'cluster_health': '🖥',
    'plex_health': '🎬',
    'stock_movers': '📈',
    'ansible_log': '⚙',
    'crucible': '📚',
}

STATUS_COLORS = {
    'healthy': '#10B981',
    'ok': '#10B981',
    'good': '#10B981',
    'degraded': '#F59E0B',
    'warning': '#F59E0B',
    'down': '#EF4444',
    'error': '#EF4444',
}


def _get_section_status(content: str) -> str:
    """Infer health-dot status class from content keywords."""
    content_lower = content.lower() if content else ''
    if any(w in content_lower for w in ['error', 'failed', 'down', 'unreachable', 'critical']):
        return 'down'
    if any(w in content_lower for w in ['warning', 'slow', 'delayed', 'stale']):
        return 'degraded'
    return 'up'


def _get_section_status_color(content: str) -> str:
    """Infer status color from content keywords (kept for compatibility)."""
    content_lower = content.lower() if content else ''
    if any(w in content_lower for w in ['error', 'failed', 'down', 'unreachable', 'critical']):
        return '#EF4444'
    if any(w in content_lower for w in ['warning', 'slow', 'delayed', 'stale']):
        return '#F59E0B'
    return '#10B981'


def generate_dashboard(sections: dict, briefing_type: str = 'morning') -> str:
    """
    Generate HTML dashboard from section data dict.
    sections: dict of {section_name: content_text}
    Returns: path to written HTML file.
    """
    now = datetime.now()
    date_str = now.strftime('%A, %B %-d')
    time_str = now.strftime('%-I:%M %p')
    emoji = '🌅' if briefing_type == 'morning' else '🌆'
    title = f'{emoji} {date_str}'

    # Build cards HTML
    cards_html = ''
    for section_name, content in sections.items():
        if not content or section_name in ('greeting', 'signoff'):
            continue
        icon = ICONS.get(section_name, '📌')
        display_name = section_name.replace('_', ' ').title()
        status = _get_section_status(str(content))
        content_escaped = html.escape(str(content)[:800]) if content else 'No data'

        cards_html += f'''
        <div class="card">
            <div style="display:flex;align-items:center;gap:10px;padding-bottom:var(--space-2);margin-bottom:var(--space-3);border-bottom:1px solid var(--border)">
                <span style="font-size:1.1em">{icon}</span>
                <span style="flex:1;font-weight:var(--weight-semibold);font-size:var(--text-sm);color:var(--text-primary)">{display_name}</span>
                <span class="health-dot {status}"></span>
            </div>
            <div style="font-size:var(--text-sm);color:var(--text-secondary);line-height:var(--leading-normal);white-space:pre-wrap;word-break:break-word">{content_escaped}</div>
        </div>'''

    # Greeting block
    greeting = sections.get('greeting', '')
    greeting_html = f'<div class="alert alert-info">{html.escape(str(greeting))}</div>' if greeting else ''

    # System status summary
    system_ok = all(
        'error' not in str(v).lower() and 'failed' not in str(v).lower()
        for k, v in sections.items()
        if k in ('cluster', 'system_status', 'cluster_health', 'today_tasks')
    )
    overall_badge = 'badge-success' if system_ok else 'badge-warning'
    overall_text = 'Systems Nominal' if system_ok else 'Attention Needed'
    overall_dot = 'up' if system_ok else 'degraded'

    page = f'''<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Lumina Briefing — {date_str}</title>
<link rel="stylesheet" href="/shared/constellation.css">
<style>
.briefing-grid{{max-width:var(--container-sm);margin:0 auto;display:flex;flex-direction:column;gap:var(--space-3)}}
.briefing-greeting{{max-width:var(--container-sm);margin:0 auto var(--space-4)}}
</style>
</head>
<body>
<div class="lumina-header">
    <div>
        <div class="lumina-logo">{title}</div>
        <div class="lumina-subtitle">Lumina Briefing · {time_str} · {briefing_type.capitalize()}</div>
    </div>
    <div style="margin-left:auto;display:flex;align-items:center;gap:var(--space-2)">
        <span class="health-dot {overall_dot}"></span>
        <span class="badge {overall_badge}">{overall_text}</span>
    </div>
</div>
<div class="page" style="max-width:var(--container-sm)">
<div class="briefing-greeting">{greeting_html}</div>
<div class="briefing-grid">
{cards_html}
</div>
</div>
<div class="lumina-footer">
    Generated by Vigil on CT310 ·
    <a href="http://YOUR_FLEET_SERVER_IP/status/" style="color:var(--accent-text)">System Status</a> ·
    <a href="http://YOUR_FLEET_SERVER_IP/research/" style="color:var(--accent-text)">Research</a>
</div>
</body>
</html>'''

    # Write index.html (latest)
    index_path = OUTPUT_DIR / 'index.html'
    index_path.write_text(page)

    # Archive by date
    archive_name = f'{now.strftime("%Y-%m-%d")}-{briefing_type}.html'
    (OUTPUT_DIR / archive_name).write_text(page)

    return str(index_path)
