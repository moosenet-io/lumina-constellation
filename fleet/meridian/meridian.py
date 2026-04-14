#!/usr/bin/env python3
"""
Meridian — Paper Trading Sandbox. SIMULATED ONLY.
Real money: NEVER. No exchange connections. No wallet access.
"""
import os, sys, json, argparse, subprocess
from datetime import datetime, date
from pathlib import Path

# ABSOLUTE SAFETY BOUNDARY
REAL_TRADING = False
assert REAL_TRADING == False, 'This system never executes real trades.'

sys.path.insert(0, '/opt/lumina-fleet/meridian')
sys.path.insert(0, '/opt/lumina-fleet/engram')
import engram
from market_data import get_crypto_prices, get_fear_greed, get_technical_indicators, get_stock_quote, get_market_sentiment
from portfolio import load_portfolio, save_portfolio, execute_trade, log_decision, get_performance, _load_env

LITELLM_URL = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
OUTPUT_DIR = Path('/opt/lumina-fleet/meridian/output/html')
OUTPUT_DIR.mkdir(parents=True, exist_ok=True)


def analyze_and_decide(portfolio_id='default'):
    """Run market analysis and generate trade recommendation. SIMULATED."""
    import urllib.request

    portfolio = load_portfolio(portfolio_id)
    prices = get_crypto_prices(['BTC', 'ETH', 'SOL'])
    fg = get_fear_greed()
    rsi = get_technical_indicators('BTC', 'RSI')
    sentiment = get_market_sentiment('bitcoin market today')

    signals = {
        'btc_price': prices.get('BTC', {}).get('price', 0),
        'btc_24h': prices.get('BTC', {}).get('change_24h', 0),
        'fear_greed': fg.get('value', 50),
        'fear_greed_label': fg.get('label', 'Unknown'),
        'rsi_14': rsi.get('value'),
        'news_count': sentiment.get('count', 0),
        'top_headline': sentiment.get('headlines', [{}])[0].get('title', '')[:80] if sentiment.get('headlines') else '',
        'portfolio_cash_pct': (portfolio['cash_balance'] / max(portfolio.get('total_value', 10000), 1)) * 100,
    }

    prompt = f"""You are Meridian, a paper trading AI analyst. This is a SIMULATION — no real money.

Current market signals:
- BTC Price: ${signals["btc_price"]:,.0f} (24h: {signals["btc_24h"]:+.1f}%)
- Fear/Greed Index: {signals["fear_greed"]} ({signals["fear_greed_label"]})
- RSI-14: {signals["rsi_14"] or "unavailable"}
- Top headline: {signals["top_headline"] or "No data"}
- Portfolio cash available: {signals["portfolio_cash_pct"]:.0f}%

Portfolio mode: {portfolio.get("mode", "advisory")}
Portfolio return: {portfolio.get("total_return_pct", 0):+.1f}%
Open positions: {list(portfolio.get("positions", {}).keys())}

Based on these signals, provide a brief trading recommendation for this SIMULATED portfolio:
1. Action: BUY [asset] / SELL [asset] / HOLD
2. Reasoning: (2-3 sentences, reference the signals)
3. Confidence: low/medium/high
4. Position size: % of portfolio (max 25%)

Reminder: This is paper trading. Treat it as a learning exercise, not financial advice."""

    try:
        data = json.dumps({'model': 'lumina-lead', 'messages': [{'role': 'user', 'content': prompt}], 'max_tokens': 300}).encode()
        req = urllib.request.Request(f'{LITELLM_URL}/v1/chat/completions', data=data,
            headers={'Authorization': f'Bearer {LITELLM_KEY}', 'Content-Type': 'application/json'}, method='POST')
        with urllib.request.urlopen(req, timeout=60) as r:
            response = json.load(r)['choices'][0]['message']['content']
    except Exception as e:
        response = f'Analysis unavailable: {e}'

    return {'signals': signals, 'recommendation': response, 'portfolio_id': portfolio_id, 'note': 'SIMULATED'}


def generate_weekly_report(portfolio_id='default'):
    """Generate weekly HTML report at /trading/index.html. SIMULATED."""
    portfolio = load_portfolio(portfolio_id)
    prices = get_crypto_prices(['BTC', 'ETH', 'SOL'])
    perf = get_performance(portfolio, prices)
    fg = get_fear_greed()

    positions_html = ''
    for asset, pos in perf['positions'].items():
        cur_price = prices.get(asset, {}).get('price', pos['avg_entry'])
        pnl = (cur_price - pos['avg_entry']) * pos['quantity']
        pnl_pct = ((cur_price - pos['avg_entry']) / pos['avg_entry']) * 100
        color = '#10B981' if pnl >= 0 else '#EF4444'
        positions_html += f'<tr><td>{asset}</td><td>{pos["quantity"]:.4f}</td><td>${pos["avg_entry"]:,.2f}</td><td>${cur_price:,.2f}</td><td style="color:{color}">${pnl:+,.2f} ({pnl_pct:+.1f}%)</td></tr>'

    report_color = '#10B981' if perf['return_pct'] >= 0 else '#EF4444'

    html = f'''<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Meridian — SIMULATED Trading</title>
<style>
body{{font-family:system-ui,sans-serif;background:#0f0f0f;color:#e5e5e5;padding:20px;max-width:800px;margin:0 auto}}
.disclaimer{{background:#1a0a0a;border:1px solid #EF4444;border-radius:8px;padding:12px;margin-bottom:20px;color:#EF4444;font-size:.85em;text-align:center;font-weight:600}}
h1{{font-size:1.4em}}.meta{{color:#888;font-size:.85em;margin-bottom:20px}}
.stat-grid{{display:grid;grid-template-columns:repeat(3,1fr);gap:10px;margin-bottom:20px}}
.stat{{background:#1a1a1a;border-radius:8px;padding:12px;text-align:center}}
.stat-num{{font-size:1.8em;font-weight:700}}
.stat-label{{color:#888;font-size:.75em}}
table{{width:100%;border-collapse:collapse;font-size:.85em;margin-bottom:20px}}
th{{text-align:left;padding:8px;border-bottom:1px solid #333;color:#888}}
td{{padding:8px;border-bottom:1px solid #1a1a1a}}
.section{{margin-bottom:20px}}
h3{{color:#aaa;font-size:.85em;text-transform:uppercase;letter-spacing:.1em;margin-bottom:10px}}
.footer{{color:#555;font-size:.75em;text-align:center;margin-top:20px}}
</style></head><body>
<div class="disclaimer">&#9888; SIMULATED PAPER TRADING — NOT FINANCIAL ADVICE &#9888;<br>No real money. No real trades. Educational purposes only.</div>
<h1>Meridian</h1>
<div class="meta">Paper Trading Portfolio · {date.today()} · Mode: {portfolio.get("mode","advisory").upper()} · SIMULATED</div>
<div class="stat-grid">
    <div class="stat"><div class="stat-num">${perf["total_value"]:,.2f}</div><div class="stat-label">Portfolio Value</div></div>
    <div class="stat"><div class="stat-num" style="color:{report_color}">{perf["return_pct"]:+.2f}%</div><div class="stat-label">Total Return</div></div>
    <div class="stat"><div class="stat-num">{perf["win_rate"]:.0f}%</div><div class="stat-label">Win Rate ({perf["trade_count"]} trades)</div></div>
    <div class="stat"><div class="stat-num">${perf["cash_balance"]:,.2f}</div><div class="stat-label">Cash Available</div></div>
    <div class="stat"><div class="stat-num">{fg["value"]}</div><div class="stat-label">Fear/Greed: {fg["label"]}</div></div>
    <div class="stat"><div class="stat-num">{len(perf["positions"])}</div><div class="stat-label">Open Positions</div></div>
</div>
<div class="section">
<h3>Open Positions</h3>
<table><tr><th>Asset</th><th>Qty</th><th>Avg Entry</th><th>Current</th><th>P&amp;L</th></tr>
{positions_html or "<tr><td colspan='5' style='color:#555;text-align:center'>No open positions</td></tr>"}
</table></div>
<div class="footer">Meridian v1.0 · SIMULATED · CT310 · <a href="http://YOUR_FLEET_SERVER_IP/" style="color:#3B82F6">Lumina Home</a></div>
</body></html>'''

    (OUTPUT_DIR / 'index.html').write_text(html)
    return str(OUTPUT_DIR / 'index.html')


if __name__ == '__main__':
    _load_env()
    parser = argparse.ArgumentParser(description='Meridian SIMULATED paper trading')
    sub = parser.add_subparsers(dest='cmd')
    sub.add_parser('status')
    sub.add_parser('analyze')
    p = sub.add_parser('reset'); p.add_argument('--balance', type=float, default=10000)
    sub.add_parser('report')

    args = parser.parse_args()

    if args.cmd == 'status':
        p = load_portfolio()
        prices = get_crypto_prices(['BTC','ETH'])
        perf = get_performance(p, prices)
        print('SIMULATED PORTFOLIO:')
        print(json.dumps(perf, indent=2))
    elif args.cmd == 'analyze':
        result = analyze_and_decide()
        print('SIMULATED ANALYSIS:')
        print(result['recommendation'])
    elif args.cmd == 'reset':
        p = load_portfolio()
        p['cash_balance'] = args.balance
        p['starting_balance'] = args.balance
        p['positions'] = {}
        p['trade_count'] = 0
        save_portfolio(p)
        print(f'SIMULATED portfolio reset to ${args.balance:.2f}')
    elif args.cmd == 'report':
        path = generate_weekly_report()
        print(f'Report at: {path}')
    else:
        parser.print_help()
