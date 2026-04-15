import subprocess, json, os
from datetime import date, datetime

# ============================================================
# Ledger Tools — Personal Finance via Actual Budget
# terminus-host SSHes to fleet-host, calls actual-http-api directly.
# ALL tools return pre-formatted strings — no LLM synthesis needed.
# De-bloat: Python does math + formatting. LLM just presents result.
# ============================================================

LEDGER_HOST = 'root@YOUR_FLEET_SERVER_IP'
ACTUAL_URL = os.environ.get('ACTUAL_API_URL', 'http://172.17.0.1:5007')
ACTUAL_KEY = os.environ.get('ACTUAL_HTTP_API_KEY', '')
ACTUAL_BUDGET_ID = os.environ.get('ACTUAL_BUDGET_ID', '')


def _actual(endpoint, method='GET', data=None, timeout=30):
    """Call actual-http-api on fleet-host via SSH. Returns parsed JSON."""
    auth = f'-H "x-api-key: {ACTUAL_KEY}"' if ACTUAL_KEY else ''
    if ACTUAL_BUDGET_ID and not endpoint.startswith('/v1'):
        url = f'{ACTUAL_URL}/v1/budgets/{ACTUAL_BUDGET_ID}{endpoint}'
    elif not endpoint.startswith('/v1'):
        url = f'{ACTUAL_URL}/v1{endpoint}'
    else:
        url = f'{ACTUAL_URL}{endpoint}'
    if data:
        d_str = json.dumps(data).replace("'", '"')
        cmd = f"curl -s -X {method} {auth} -H 'Content-Type: application/json' -d '{d_str}' {url}"
    else:
        cmd = f"curl -s -X {method} {auth} {url}"
    full = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {LEDGER_HOST} '{cmd}'"
    result = subprocess.run(full, shell=True, capture_output=True, text=True, timeout=timeout)
    if not result.stdout.strip():
        return {'error': result.stderr[:200] or 'No response from Actual Budget API'}
    try:
        return json.loads(result.stdout)
    except Exception:
        return {'raw': result.stdout[:300]}


def _fmt_currency(cents):
    """Convert cents (integer) to dollars string."""
    if cents is None:
        return '$0.00'
    return f'${abs(cents) / 100:.2f}'


def _no_budget():
    return 'Actual Budget not configured. Open http://YOUR_FLEET_SERVER_IP:5006 to set up.'


def register_ledger_tools(mcp):

    @mcp.tool()
    def ledger_budget_status() -> str:
        """Get current month budget status — formatted summary with spend, remaining, alerts.
        Returns: human-readable string with total spent, budgeted, % used, top categories.
        No LLM synthesis needed — call this and present the result directly."""
        if not ACTUAL_BUDGET_ID:
            return _no_budget()
        month = date.today().strftime('%Y-%m')
        month_display = date.today().strftime('%B %Y')

        data = _actual(f'/months/{month}')
        if 'error' in data:
            return f'Budget error: {data["error"]}'

        d = data.get('data', {})
        total_spent = abs(d.get('totalSpent', 0))
        total_budgeted = d.get('totalBudgeted', 0)
        to_budget = d.get('toBudget', 0)

        # Category breakdown — fetch category groups for top spenders
        cats = _actual('/categories')
        cat_data = cats.get('data', []) if isinstance(cats.get('data'), list) else []

        # Build category summary
        cat_rows = []
        alerts = []
        for cat in cat_data:
            spent = abs(cat.get('spent', 0))
            budgeted = cat.get('budgeted', 0)
            name = cat.get('name', '?')
            if spent == 0 and budgeted == 0:
                continue
            pct = round(spent / budgeted * 100) if budgeted > 0 else 0
            cat_rows.append((spent, name, budgeted, pct))
            if pct >= 80 and budgeted > 0:
                alerts.append(f'{name} at {pct}%')

        cat_rows.sort(reverse=True)
        top3 = ', '.join(f'{n} ({_fmt_currency(s)})' for s, n, b, p in cat_rows[:3]) or 'no categories set'

        if total_budgeted == 0:
            summary = (f'Budget status for {month_display}: '
                       f'No budget categories configured yet. '
                       f'Total tracked: {_fmt_currency(total_spent)}. '
                       f'Set up categories at http://YOUR_FLEET_SERVER_IP:5006 to track spending.')
        else:
            pct_used = round(total_spent / total_budgeted * 100) if total_budgeted > 0 else 0
            avail = _fmt_currency(abs(to_budget))
            sign = 'available' if to_budget >= 0 else 'over budget'
            summary = (f'Budget for {month_display}: '
                       f'{_fmt_currency(total_spent)} spent of {_fmt_currency(total_budgeted)} '
                       f'({pct_used}% used). '
                       f'{avail} {sign}. '
                       f'Top categories: {top3}.')
            if alerts:
                summary += f' ⚠️ Over 80%: {", ".join(alerts)}.'

        return summary

    @mcp.tool()
    def ledger_transactions(account_id: str = '', limit: int = 10) -> str:
        """Get recent transactions — formatted list with date, payee, amount, category.
        Returns: human-readable string. No LLM synthesis needed."""
        if not ACTUAL_BUDGET_ID:
            return _no_budget()
        if account_id:
            raw = _actual(f'/accounts/{account_id}/transactions?limit={limit}')
        else:
            raw = _actual(f'/transactions?limit={limit}')

        if 'error' in raw:
            return f'Transaction fetch error: {raw["error"]}'

        txns = raw.get('data', [])
        if not txns:
            return 'No transactions found. Connect a bank account in Actual Budget to start tracking.'

        lines = [f'Last {len(txns)} transactions:']
        for t in txns[:limit]:
            dt = t.get('date', '?')[:10]
            payee = t.get('payee_name', t.get('imported_payee', 'Unknown'))[:30]
            amount = t.get('amount', 0)  # negative = expense in cents
            cat = t.get('category_name', '')
            amt_str = _fmt_currency(abs(amount))
            direction = '-' if amount < 0 else '+'
            cat_str = f' [{cat}]' if cat else ''
            lines.append(f'  {dt}: {payee}{cat_str} {direction}{amt_str}')

        return '\n'.join(lines)

    @mcp.tool()
    def ledger_accounts() -> str:
        """List Actual Budget accounts with current balances.
        Returns: formatted account list. No LLM synthesis needed."""
        if not ACTUAL_BUDGET_ID:
            return _no_budget()
        raw = _actual('/accounts')
        if 'error' in raw:
            return f'Accounts error: {raw["error"]}'

        accounts = raw.get('data', [])
        if not accounts:
            return 'No accounts found. Add accounts in Actual Budget at http://YOUR_FLEET_SERVER_IP:5006'

        lines = ['Accounts:']
        total = 0
        for acc in accounts:
            name = acc.get('name', '?')
            balance = acc.get('balance', 0)
            closed = ' (closed)' if acc.get('closed') else ''
            lines.append(f'  {name}{closed}: {_fmt_currency(balance)}')
            total += balance
        lines.append(f'  Total: {_fmt_currency(total)}')
        return '\n'.join(lines)

    @mcp.tool()
    def ledger_categories() -> str:
        """List budget categories with IDs for logging transactions.
        Returns: formatted category list with IDs. No LLM synthesis needed."""
        if not ACTUAL_BUDGET_ID:
            return _no_budget()
        raw = _actual('/categories')
        if 'error' in raw:
            return f'Categories error: {raw["error"]}'

        cats = raw.get('data', [])
        if not cats:
            return 'No categories set up yet. Create categories in Actual Budget.'

        lines = ['Categories (use ID when logging transactions):']
        for cat in cats:
            cid = cat.get('id', '?')
            name = cat.get('name', '?')
            budgeted = cat.get('budgeted', 0)
            bstr = f' [budget: {_fmt_currency(budgeted)}]' if budgeted else ''
            lines.append(f'  {name}: {cid}{bstr}')
        return '\n'.join(lines)

    @mcp.tool()
    def ledger_log(amount: float, payee: str, category_id: str = '', account_id: str = '', notes: str = '') -> str:
        """Log an expense. amount: positive number (e.g. 45.50 for a $45.50 expense).
        Returns: confirmation string. Use ledger_categories() to find category IDs."""
        if not ACTUAL_BUDGET_ID:
            return _no_budget()
        transaction = {
            'date': date.today().isoformat(),
            'amount': int(amount * -100),  # negative cents for expenses
            'payee_name': payee[:50],
            'notes': notes,
        }
        if category_id:
            transaction['category'] = category_id
        if account_id:
            transaction['account'] = account_id

        result = _actual('/transactions', 'POST', {'transactions': [transaction]})
        if 'error' in result:
            return f'Failed to log transaction: {result["error"]}'

        txns = result.get('data', {}).get('added', [])
        if txns:
            return f'Logged: ${amount:.2f} to {payee} on {date.today().isoformat()}.'
        return f'Transaction submitted for {payee} (${amount:.2f}). Check Actual Budget to confirm.'

    @mcp.tool()
    def ledger_get_budget_id() -> str:
        """List available Actual Budget files and their sync IDs.
        Run this to find the budget_id to configure in .env."""
        raw = _actual('/v1/budgets', method='GET')
        if 'error' in raw:
            return f'Could not list budgets: {raw["error"]}'

        budgets = raw.get('data', [])
        if not budgets:
            return 'No budgets found. Open http://YOUR_FLEET_SERVER_IP:5006 and create a budget first.'

        lines = ['Available budgets (use groupId as ACTUAL_BUDGET_ID):']
        for b in budgets:
            name = b.get('name', '?')
            gid = b.get('groupId', '?')
            lines.append(f'  {name}: {gid}')
        return '\n'.join(lines)
