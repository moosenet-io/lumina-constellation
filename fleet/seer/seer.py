#!/usr/bin/env python3
"""Seer research engine — multi-source web research with injection sanitization."""

import sys as _sys; _sys.path.insert(0, '/opt/lumina-fleet')
try: from naming import display_name as _dn
except: _dn = lambda x: x

import os, sys, json, uuid, time, argparse, html, html.parser
import urllib.request, urllib.parse
from datetime import datetime
from sanitizer import score_and_decide

LITELLM_URL = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
SEARXNG_URL = os.environ.get('SEARXNG_URL', 'http://YOUR_SEARXNG_IP:8088')
OUTPUT_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'output')
GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')
GITEA_URL = os.environ.get('GITEA_URL', 'http://YOUR_GITEA_IP:3000')

QWEN_MODEL = os.environ.get('SEER_QWEN_MODEL', 'local-qwen2.5-7b-gpu')

EFFORT_CONFIG = {
    'light':    {'terms': 3, 'results': 5,  'gap_loops': 0, 'synth_model': QWEN_MODEL,          'passes': 1},
    'standard': {'terms': 5, 'results': 10, 'gap_loops': 1, 'synth_model': 'claude-haiku-4-5',  'passes': 1},
    'deep':     {'terms': 7, 'results': 20, 'gap_loops': 3, 'synth_model': 'claude-sonnet-4-6', 'passes': 3},
}

class HTMLStripper(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.text = []
    def handle_data(self, d):
        self.text.append(d)
    def get_text(self):
        return ' '.join(self.text)

def strip_html(text):
    s = HTMLStripper()
    try:
        s.feed(text)
        return s.get_text()
    except:
        return text

def llm_call(prompt, model=None, max_tokens=500, system=None):
    if model is None:
        model = QWEN_MODEL
    messages = []
    if system:
        messages.append({'role': 'system', 'content': system})
    messages.append({'role': 'user', 'content': prompt})
    data = json.dumps({'model': model, 'messages': messages, 'max_tokens': max_tokens, 'temperature': 0.3}).encode()
    req = urllib.request.Request(
        f'{LITELLM_URL}/v1/chat/completions', data=data,
        headers={'Authorization': f'Bearer {LITELLM_KEY}', 'Content-Type': 'application/json'}, method='POST')
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)['choices'][0]['message']['content'].strip()

def plan_query(query, n_terms):
    result = llm_call(
        f'Decompose this research question into exactly {n_terms} distinct search terms that would return complementary results. Return one search term per line, no numbering or bullets.\n\nQuestion: {query}',
        max_tokens=200
    )
    terms = [t.strip() for t in result.strip().split('\n') if t.strip()]
    return terms[:n_terms]

def searxng_search(term, max_results):
    params = urllib.parse.urlencode({"q": term, "format": "json", "categories": "general"})
    url = f'{SEARXNG_URL}/search?{params}'
    try:
        req = urllib.request.Request(url, headers={'User-Agent': f'{_dn("seer")}/1.0'})
        with urllib.request.urlopen(req, timeout=10) as r:
            data = json.load(r)
        return data.get('results', [])[:max_results]
    except Exception as e:
        print(f'[{_dn("seer").lower()}] SearXNG error for "{term}": {e}', file=sys.stderr)
        return []

def fetch_content(url, max_bytes=50000):
    try:
        req = urllib.request.Request(url, headers={'User-Agent': f'{_dn("seer")}/1.0'})
        with urllib.request.urlopen(req, timeout=10) as r:
            raw = r.read(max_bytes).decode('utf-8', errors='replace')
        return strip_html(raw)[:8000]
    except:
        return None

def research(query, effort='standard', focus='overview', max_sources=None, report_id=None):
    if report_id is None:
        report_id = str(uuid.uuid4())
    
    cfg = EFFORT_CONFIG.get(effort, EFFORT_CONFIG['standard'])
    n_terms = cfg['terms']
    n_results = max_sources or cfg['results']
    
    print(f'[{_dn("seer").lower()}] {report_id[:8]} | query: {query[:60]} | effort: {effort}')
    
    # Step 1: Plan
    terms = plan_query(query, n_terms)
    print(f'[{_dn("seer").lower()}] Search terms: {terms}')
    
    all_results = []
    seen_urls = set()
    
    for loop in range(cfg['gap_loops'] + 1):
        # Step 2: Search
        for term in terms:
            results = searxng_search(term, n_results)
            for r in results:
                if r.get('url') not in seen_urls:
                    seen_urls.add(r['url'])
                    all_results.append(r)
        
        # Step 3-5: Fetch, sanitize, summarize
        summaries = []
        excluded = []
        for result in all_results[:n_results * 2]:
            content = fetch_content(result['url']) or result.get('content', '')
            if not content:
                continue
            san = score_and_decide(content, query, LITELLM_URL, LITELLM_KEY)
            if san['action'] == 'exclude':
                excluded.append({'url': result['url'], 'reason': san['flags']})
                continue
            summary = san.get('summary') or llm_call(
                f'Summarize this content about "{query}" in 3-5 sentences:\n\n{content[:4000]}',
                max_tokens=200
            )
            summaries.append({
                'url': result['url'],
                'title': result.get('title', ''),
                'summary': summary,
                'warning': san['action'] == 'warn',
                'san_score': san['score']
            })
            if len(summaries) >= n_results:
                break
        
        # Step 6: Gap analysis (if loops remain)
        if loop < cfg['gap_loops'] and summaries:
            gap_prompt = f'Given these research summaries about "{query}", what 2-3 important perspectives are missing? Return specific search terms, one per line.'
            gap_terms = llm_call(gap_prompt + '\n\nSummaries:\n' + '\n'.join(s['summary'] for s in summaries[:5]), max_tokens=150)
            terms = [t.strip() for t in gap_terms.split('\n') if t.strip()][:3]
            print(f'[{_dn("seer").lower()}] Gap terms: {terms}')
    
    # Step 7: Synthesis
    summary_text = '\n\n'.join(f"[{s['title']}] ({s['url']})\n{s['summary']}" for s in summaries)
    
    if cfg['passes'] == 1:
        report_body = llm_call(
            f'Write a research report on: {query}\n\nFocus: {focus}\n\nSource summaries:\n{summary_text[:8000]}',
            model=cfg['synth_model'], max_tokens=1500,
            system='You are a research analyst. Write a structured report with sections: Overview, Key Findings, Sources. Be precise and cite sources.'
        )
    else:
        # Deep tier: 3 passes
        draft = llm_call(f'Write a draft research report on: {query}\n\nSummaries:\n{summary_text[:6000]}', model=cfg['synth_model'], max_tokens=1500)
        critique = llm_call(f'Review this draft report. Identify contradictions, weak evidence, and missing perspectives:\n\n{draft}', model=cfg['synth_model'], max_tokens=500)
        report_body = llm_call(f'Refine this report based on the critique. Add confidence ratings and an executive summary with recommendation.\n\nDraft:\n{draft}\n\nCritique:\n{critique}', model=cfg['synth_model'], max_tokens=2000)
    
    # Step 8: Publish
    date_str = datetime.now().strftime('%Y-%m-%d')
    slug = query[:40].lower().replace(' ', '-').replace('?', '').replace('/', '-')
    
    # Markdown
    md_path = os.path.join(OUTPUT_DIR, 'markdown', f'{date_str}-{slug}.md')
    os.makedirs(os.path.dirname(md_path), exist_ok=True)
    md_content = f'# {query}\n\n*{date_str} | effort: {effort} | sources: {len(summaries)}*\n\n{report_body}\n\n## Sources\n' + \
                 '\n'.join(f'- [{s["title"] or s["url"]}]({s["url"]}){" (unverified)" if s["warning"] else ""}' for s in summaries)
    if excluded:
        md_content += f'\n\n## Excluded sources ({len(excluded)})\n' + '\n'.join(f'- {e["url"]}: {e["reason"][:60]}' for e in excluded)
    
    with open(md_path, 'w') as f:
        f.write(md_content)
    
    # HTML
    html_path = os.path.join(OUTPUT_DIR, 'html', 'research', f'{date_str}-{slug}.html')
    os.makedirs(os.path.dirname(html_path), exist_ok=True)
    html_content = f'''<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1.0">
<title>{html.escape(query)}</title>
<link rel="stylesheet" href="/shared/constellation.css">
<style>
  body {{ padding: 2rem; }}
  .report-body {{ line-height: 1.7; white-space: pre-wrap; word-break: break-word; }}
  .sources-list {{ list-style: none; padding: 0; margin: 0; }}
  .sources-list li {{ padding: 0.35rem 0; border-bottom: 1px solid var(--bg-tertiary); font-size: 0.875rem; }}
  .sources-list li:last-child {{ border-bottom: none; }}
</style>
</head>
<body>
<div style="max-width:860px;margin:0 auto;">
  <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:1.5rem;">
    <h1 style="font-size:1.5rem;font-weight:700;margin:0;">&#x1F50D; {html.escape(query)}</h1>
    <div style="font-size:0.75rem;color:var(--text-tertiary);">{date_str}</div>
  </div>
  <div style="font-size:0.8rem;color:var(--text-secondary);margin-bottom:1.5rem;">
    Effort: {effort} &bull; {len(summaries)} sources &bull; {len(excluded)} excluded
  </div>
  <div class="card" style="padding:1.5rem;margin-bottom:1.5rem;">
    <div class="report-body">{report_body.replace(chr(10), "<br>")}</div>
  </div>
  <div class="card" style="padding:1rem;">
    <h2 style="font-size:1rem;font-weight:700;margin:0 0 0.75rem 0;">Sources</h2>
    <ul class="sources-list">
{"".join(f'<li><a href="{html.escape(s["url"])}" style="color:var(--accent);">{html.escape(s["title"] or s["url"])}</a>{" <span style=\\"color:var(--color-warning);font-size:0.75rem;\\">(unverified)</span>" if s["warning"] else ""}</li>' for s in summaries)}
    </ul>
{"<h3 style=\\"font-size:0.875rem;margin:1rem 0 0.5rem 0;\\">Excluded</h3><ul class=\\"sources-list\\">" + "".join(f'<li style=\\"color:var(--color-error);font-size:0.75rem;\\">{html.escape(e["url"])}: {html.escape(str(e["reason"])[:60])}</li>' for e in excluded) + "</ul>" if excluded else ""}
  </div>
  <div style="margin-top:1.5rem;text-align:center;font-size:0.75rem;color:var(--text-tertiary);">
    Lumina Constellation &middot; Seer Research &middot; <a href="/status" style="color:var(--accent);">Dashboard</a>
  </div>
</div>
</body></html>'''
    
    with open(html_path, 'w') as f:
        f.write(html_content)
    
    url = f'http://YOUR_FLEET_SERVER_IP/research/{date_str}-{slug}.html'
    print(f'[{_dn("seer").lower()}] Complete. Report: {url}')
    
    return {
        'report_id': report_id,
        'status': 'complete',
        'path': f'research/{date_str}-{slug}.html',
        'url': url,
        'markdown_path': md_path,
        'summary': report_body[:300],
        'sources_used': len(summaries),
        'sources_excluded': len(excluded),
        'effort': effort,
        'query': query
    }

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--query', required=True)
    parser.add_argument('--effort', default='standard', choices=['light','standard','deep'])
    parser.add_argument('--focus', default='overview')
    args = parser.parse_args()
    result = research(args.query, args.effort, args.focus)
    print(json.dumps(result, indent=2))
