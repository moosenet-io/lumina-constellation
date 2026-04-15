#!/usr/bin/env python3
"""
report_template.py — Self-contained HTML report generator for Lumina Constellation modules.

Usage:
    from soma.report_template import Report

    r = Report(
        title='Sentinel Health Check',
        module='sentinel',
        metadata={'model': 'python-only', 'cost': '$0.00', 'host': 'fleet-host'}
    )
    r.add_kpi('Services healthy', '11', style='success')
    r.add_kpi('Services down', '0', style='error')
    r.add_section('Service Status', r.table(
        headers=['Service', 'Status', 'Latency'],
        rows=[['IronClaw', 'OK', '12ms'], ['Nexus', 'OK', '3ms']]
    ))
    path = r.save()  # saves to /opt/lumina-fleet/reports/sentinel/
    print(f'Report saved to {path}')
"""

import os
from pathlib import Path
from datetime import datetime


REPORTS_BASE = Path(os.environ.get('LUMINA_REPORTS_DIR', '/opt/lumina-fleet/reports'))

# Inline the reports CSS for self-contained files
_REPORTS_CSS_URL = 'http://YOUR_FLEET_SERVER_IP/shared/constellation.css'
_REPORTS_CSS_EXTRA_URL = 'http://YOUR_FLEET_SERVER_IP/soma/static/constellation-reports.css'


class Report:
    """Builder for self-contained Lumina HTML reports."""

    def __init__(self, title: str, module: str, metadata: dict = None):
        self.title = title
        self.module = module
        self.metadata = metadata or {}
        self.timestamp = datetime.now()
        self._kpis: list[dict] = []
        self._sections: list[tuple[str, str]] = []

    def add_kpi(self, label: str, value: str, style: str = '') -> 'Report':
        """Add a KPI card to the report header summary."""
        self._kpis.append({'label': label, 'value': value, 'style': style})
        return self

    def add_section(self, heading: str, html_content: str) -> 'Report':
        """Add a named section with arbitrary HTML content."""
        self._sections.append((heading, html_content))
        return self

    def table(self, headers: list[str], rows: list[list], row_classes: list[str] = None) -> str:
        """Generate HTML table markup."""
        th = ''.join(f'<th>{h}</th>' for h in headers)
        tr_rows = []
        for i, row in enumerate(rows):
            cls = (row_classes[i] if row_classes and i < len(row_classes) else '')
            tds = ''.join(f'<td>{cell}</td>' for cell in row)
            tr_rows.append(f'<tr class="{cls}">{tds}</tr>')
        return f'<table class="report-table"><thead><tr>{th}</tr></thead><tbody>{"".join(tr_rows)}</tbody></table>'

    def bar_chart(self, items: list[tuple[str, float, float, str]]) -> str:
        """Generate CSS-only bar chart. items = [(label, value, max, color_class)]"""
        bars = []
        for label, value, max_val, color_class in items:
            pct = min(100, int((value / max_val) * 100)) if max_val > 0 else 0
            bars.append(f'''
            <div class="report-bar-row">
              <span class="report-bar-label">{label}</span>
              <div class="report-bar-track">
                <div class="report-bar-fill {color_class}" style="width:{pct}%"></div>
              </div>
              <span class="report-bar-value">{value}</span>
            </div>''')
        return f'<div class="report-chart-container"><div class="report-bar-chart">{"".join(bars)}</div></div>'

    def model_response(self, model: str, content: str) -> str:
        """Format a model's response in a council report."""
        model_key = model.lower().split('-')[0].split('/')[0]
        tag = f'<span class="report-model-tag model-{model_key}">{model}</span>'
        return f'<div class="report-model-response model-{model_key}">{tag}<div style="margin-top:0.5rem;">{content}</div></div>'

    def _build_html(self) -> str:
        # KPI grid
        kpi_html = ''
        if self._kpis:
            cards = ''.join(
                f'<div class="report-kpi-card kpi-{k.get("style","")}">'
                f'<div class="kpi-value">{k["value"]}</div>'
                f'<div class="kpi-label">{k["label"]}</div>'
                f'</div>'
                for k in self._kpis
            )
            kpi_html = f'<div class="report-kpi-grid">{cards}</div>'

        # Sections
        section_html = ''
        for heading, content in self._sections:
            section_html += f'<div class="report-section"><h2>{heading}</h2>{content}</div>'

        # Metadata tags
        meta_tags = ''.join(
            f'<span>{k}: <strong>{v}</strong></span>'
            for k, v in self.metadata.items()
        )

        ts_str = self.timestamp.strftime('%Y-%m-%d %H:%M UTC')

        return f'''<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{self.title} — Lumina</title>
<link rel="stylesheet" href="{_REPORTS_CSS_URL}">
<link rel="stylesheet" href="{_REPORTS_CSS_EXTRA_URL}">
</head>
<body>
<div class="report-container">
  <div class="report-header">
    <h1>{self.title}</h1>
    <div class="report-metadata">
      <span>&#128197; {ts_str}</span>
      <span>&#128230; {self.module}</span>
      {meta_tags}
    </div>
  </div>
  {kpi_html}
  {section_html}
  <div class="report-footer">
    <span>Lumina Constellation &middot; {self.module}</span>
    <span>Generated {ts_str}</span>
  </div>
</div>
</body>
</html>'''

    def save(self, filename: str = None, output_dir: Path = None) -> Path:
        """Save the report to the reports directory. Returns the path."""
        if output_dir is None:
            output_dir = REPORTS_BASE / self.module
        output_dir = Path(output_dir)
        output_dir.mkdir(parents=True, exist_ok=True)

        if filename is None:
            ts = self.timestamp.strftime('%Y%m%d-%H%M%S')
            slug = self.title.lower().replace(' ', '-').replace('/', '-')[:30]
            filename = f'{ts}-{slug}.html'

        path = output_dir / filename
        path.write_text(self._build_html())
        return path

    def html(self) -> str:
        """Return the complete HTML as a string without saving."""
        return self._build_html()
