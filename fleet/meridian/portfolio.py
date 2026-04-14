"""
Meridian virtual portfolio engine. SIMULATED ONLY — no real trades ever.
Safety: REAL_TRADING = False is hardcoded and cannot be overridden.
"""
import os, sys, json, time, uuid
from datetime import datetime, date
from pathlib import Path

# SAFETY BOUNDARY — NEVER SET TO TRUE
REAL_TRADING = False
if REAL_TRADING:
    raise RuntimeError('REAL_TRADING must always be False in Meridian. This is a simulation.')

sys.path.insert(0, '/opt/lumina-fleet/engram')
import engram

PORTFOLIO_KEY = 'meridian/portfolio/default'
JOURNAL_KEY_PREFIX = 'meridian/journal'
MAX_POSITION_PCT = 0.25  # max 25% per trade
MAX_DAILY_TRADES = 5
FEE_PCT = 0.001  # 0.1%
SLIPPAGE_PCT = 0.0005  # 0.05%


def _load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip())
    engram.LLM_KEY = os.environ.get('LITELLM_MASTER_KEY', engram.LLM_KEY)


def load_portfolio(portfolio_id='default'):
    """Load portfolio state from Engram."""
    import sqlite3
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    try:
        row = conn.execute('SELECT content FROM knowledge_base WHERE key=?', (f'meridian/portfolio/{portfolio_id}',)).fetchone()
    except Exception:
        row = None
    conn.close()
    if row:
        return json.loads(row[0])
    # Default starting portfolio
    return {
        'portfolio_id': portfolio_id, 'created': date.today().isoformat(),
        'starting_balance': 10000.00, 'cash_balance': 10000.00,
        'positions': {}, 'trade_count': 0, 'daily_trades': {},
        'mode': 'advisory', 'total_value': 10000.00, 'total_return_pct': 0.0,
        'win_count': 0, 'loss_count': 0,
        'note': 'SIMULATED — not financial advice'
    }

def save_portfolio(portfolio):
    """Save portfolio state to Engram."""
    portfolio['last_updated'] = datetime.utcnow().isoformat() + 'Z'
    engram.store(f'meridian/portfolio/{portfolio["portfolio_id"]}', json.dumps(portfolio),
                layer='kb', tags=['meridian', 'portfolio', 'SIMULATED'])

def get_portfolio_value(portfolio, current_prices):
    """Calculate total portfolio value including positions."""
    total = portfolio['cash_balance']
    for asset, pos in portfolio.get('positions', {}).items():
        price = current_prices.get(asset, {}).get('price', pos['avg_entry'])
        total += pos['quantity'] * price
    return round(total, 2)

def check_trade_limits(portfolio, action, asset, quantity, price):
    """Enforce trading limits. Returns (ok, reason)."""
    today = date.today().isoformat()
    daily = portfolio.get('daily_trades', {}).get(today, 0)
    if daily >= MAX_DAILY_TRADES:
        return False, f'Daily trade limit reached ({MAX_DAILY_TRADES}/day)'

    if action == 'buy':
        cost = quantity * price * (1 + FEE_PCT + SLIPPAGE_PCT)
        if cost > portfolio['cash_balance']:
            return False, f'Insufficient cash: need ${cost:.2f}, have ${portfolio["cash_balance"]:.2f}'
        total_val = portfolio.get('total_value', 10000)
        if cost > total_val * MAX_POSITION_PCT:
            return False, f'Position too large: ${cost:.2f} > {MAX_POSITION_PCT*100:.0f}% of portfolio'
    elif action == 'sell':
        pos = portfolio.get('positions', {}).get(asset, {})
        if not pos or pos.get('quantity', 0) < quantity:
            return False, f'Insufficient {asset}: have {pos.get("quantity",0)}, selling {quantity}'

    return True, 'ok'

def execute_trade(portfolio, action, asset, quantity, price):
    """Execute a SIMULATED trade. Updates portfolio state."""
    assert REAL_TRADING == False, 'Safety check failed'

    ok, reason = check_trade_limits(portfolio, action, asset, quantity, price)
    if not ok:
        return {'status': 'rejected', 'reason': reason, 'note': 'SIMULATED'}

    fee = quantity * price * FEE_PCT
    slip = quantity * price * SLIPPAGE_PCT
    today = date.today().isoformat()

    if action == 'buy':
        total_cost = quantity * price + fee + slip
        portfolio['cash_balance'] -= total_cost
        pos = portfolio.setdefault('positions', {}).get(asset, {'quantity': 0, 'avg_entry': 0, 'total_cost': 0})
        new_qty = pos['quantity'] + quantity
        pos['avg_entry'] = (pos['total_cost'] + total_cost) / new_qty if new_qty > 0 else price
        pos['quantity'] = new_qty
        pos['total_cost'] = pos.get('total_cost', 0) + total_cost
        portfolio['positions'][asset] = pos
        cash_change = -total_cost
    else:  # sell
        pos = portfolio['positions'].get(asset, {})
        proceeds = quantity * price - fee - slip
        pnl = proceeds - (pos.get('avg_entry', price) * quantity)
        portfolio['cash_balance'] += proceeds
        pos['quantity'] -= quantity
        if pos['quantity'] <= 0:
            del portfolio['positions'][asset]
        else:
            portfolio['positions'][asset] = pos
        if pnl > 0: portfolio['win_count'] = portfolio.get('win_count', 0) + 1
        else: portfolio['loss_count'] = portfolio.get('loss_count', 0) + 1
        cash_change = proceeds

    portfolio['trade_count'] = portfolio.get('trade_count', 0) + 1
    portfolio['daily_trades'][today] = portfolio.get('daily_trades', {}).get(today, 0) + 1

    receipt = {
        'status': 'executed', 'action': action, 'asset': asset,
        'quantity': quantity, 'price': price, 'fee': round(fee, 2),
        'slippage': round(slip, 2), 'cash_change': round(cash_change, 2),
        'cash_remaining': round(portfolio['cash_balance'], 2),
        'timestamp': datetime.utcnow().isoformat() + 'Z',
        'note': 'SIMULATED — not a real trade'
    }
    return receipt

def log_decision(portfolio_id, action, asset, quantity, price, signals, reasoning):
    """Log a trade decision to Engram reasoning journal."""
    entry = {
        'id': str(uuid.uuid4()), 'timestamp': datetime.utcnow().isoformat() + 'Z',
        'portfolio_id': portfolio_id, 'action': action, 'asset': asset,
        'quantity': quantity, 'price': price, 'signals': signals,
        'reasoning': reasoning, 'outcome': None, 'note': 'SIMULATED'
    }
    key = f'meridian/journal/{entry["id"]}'
    engram.store(key, json.dumps(entry), layer='kb', tags=['meridian', 'journal', 'SIMULATED', asset])
    engram.journal(agent='meridian', action=f'SIMULATED {action} {asset}',
                  outcome=f'{quantity} @ ${price} — {reasoning[:80]}', context='paper_trading')
    return entry['id']

def get_performance(portfolio, current_prices):
    """Calculate performance vs benchmarks."""
    total = get_portfolio_value(portfolio, current_prices)
    portfolio['total_value'] = total
    ret_pct = ((total - portfolio['starting_balance']) / portfolio['starting_balance']) * 100
    portfolio['total_return_pct'] = round(ret_pct, 2)

    trades = portfolio.get('trade_count', 0)
    wins = portfolio.get('win_count', 0)
    win_rate = (wins / trades * 100) if trades > 0 else 0

    return {
        'total_value': total, 'starting_balance': portfolio['starting_balance'],
        'cash_balance': round(portfolio['cash_balance'], 2),
        'return_pct': round(ret_pct, 2), 'trade_count': trades,
        'win_rate': round(win_rate, 1), 'positions': portfolio.get('positions', {}),
        'note': 'SIMULATED — not financial advice'
    }

if __name__ == '__main__':
    _load_env()
    import sys
    if len(sys.argv) > 1:
        cmd = sys.argv[1]
        p = load_portfolio()
        if cmd == 'status':
            print('SIMULATED PORTFOLIO STATUS:')
            print(json.dumps({k: v for k, v in p.items() if k != 'daily_trades'}, indent=2))
        elif cmd == 'reset':
            bal = float(sys.argv[2]) if len(sys.argv) > 2 else 10000.0
            p = {'portfolio_id': 'default', 'created': date.today().isoformat(),
                 'starting_balance': bal, 'cash_balance': bal, 'positions': {},
                 'trade_count': 0, 'daily_trades': {}, 'mode': 'advisory',
                 'total_value': bal, 'total_return_pct': 0.0, 'win_count': 0, 'loss_count': 0,
                 'note': 'SIMULATED — not financial advice'}
            save_portfolio(p)
            print(f'SIMULATED portfolio reset to ${bal:.2f}')
