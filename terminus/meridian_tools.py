"""
meridian_tools.py — MCP tools for Meridian paper trading sandbox.
SIMULATED ONLY — no real trades ever executed.
All tools SSH to fleet-host and return results with SIMULATED label.
"""
import subprocess
import json
import os

# SAFETY BOUNDARY — NEVER CHANGE
REAL_TRADING = False


def _run_fleet(cmd: str, timeout: int = 60) -> str:
    """Run a command on the configured fleet target via SSH."""
    remote_host = os.environ.get("REMOTE_SSH_HOST", "")
    fleet_target = os.environ.get("FLEET_REMOTE_TARGET", "")
    template = os.environ.get("REMOTE_EXEC_TEMPLATE", "")
    if not (remote_host and fleet_target and template):
        return "ERROR: remote fleet access not configured"
    command = f"bash -c 'set -a && source /opt/lumina-fleet/axon/.env && set +a && cd /opt/lumina-fleet/meridian && {cmd}' 2>&1"
    remote_cmd = template.format(target=fleet_target, command=command)
    full_cmd = ["ssh", remote_host, remote_cmd]
    try:
        result = subprocess.run(full_cmd, capture_output=True, text=True, timeout=timeout)
        output = result.stdout + result.stderr
        return output.strip() if output.strip() else '(no output)'
    except subprocess.TimeoutExpired:
        return f'ERROR: command timed out after {timeout}s'
    except Exception as e:
        return f'ERROR: {e}'


def register_meridian_tools(mcp):
    """Register Meridian paper trading MCP tools."""

    @mcp.tool()
    def meridian_portfolio(portfolio_id: str = 'default') -> dict:
        """
        Get current SIMULATED paper trading portfolio status.
        Returns portfolio value, positions, cash balance, and performance metrics.
        This is a paper trading simulation — no real money involved.
        """
        assert REAL_TRADING == False, 'Safety check: real trading is disabled'
        output = _run_fleet('python3 meridian.py status')
        try:
            # Parse the JSON after "SIMULATED PORTFOLIO:" line
            lines = output.splitlines()
            json_start = next((i for i, l in enumerate(lines) if l.strip().startswith('{')), None)
            if json_start is not None:
                data = json.loads('\n'.join(lines[json_start:]))
                data['_note'] = 'SIMULATED — not financial advice'
                return data
        except Exception:
            pass
        return {'output': output, '_note': 'SIMULATED — not financial advice'}

    @mcp.tool()
    def meridian_analysis(portfolio_id: str = 'default') -> dict:
        """
        Run SIMULATED market analysis and get AI trade recommendation.
        Fetches live crypto prices, Fear/Greed index, and news sentiment,
        then asks the LLM for a paper trading recommendation.
        SIMULATED ONLY — output is educational, not financial advice.
        """
        assert REAL_TRADING == False, 'Safety check: real trading is disabled'
        output = _run_fleet('python3 meridian.py analyze', timeout=90)
        return {
            'analysis': output,
            '_note': 'SIMULATED — not financial advice. Paper trading only.'
        }

    @mcp.tool()
    def meridian_report() -> dict:
        """
        Generate the SIMULATED Meridian trading dashboard HTML report.
        Publishes to http://YOUR_FLEET_SERVER_IP/trading/index.html
        Returns the path and URL of the generated report.
        SIMULATED ONLY.
        """
        assert REAL_TRADING == False, 'Safety check: real trading is disabled'
        output = _run_fleet('python3 meridian.py report')
        return {
            'status': 'generated' if 'Report at:' in output else 'error',
            'url': 'http://YOUR_FLEET_SERVER_IP/trading/',
            'output': output,
            '_note': 'SIMULATED — not financial advice'
        }

    @mcp.tool()
    def meridian_market_data(symbols: str = 'BTC,ETH,SOL') -> dict:
        """
        Fetch live market data for the SIMULATED Meridian trading system.
        Returns crypto prices (CoinGecko), Fear/Greed index, and SPY quote.
        symbols: comma-separated list (BTC, ETH, SOL, BNB, AVAX supported).
        Read-only market data — SIMULATED context only.
        """
        sym_list = [s.strip() for s in symbols.split(',')]
        sym_json = json.dumps(sym_list)
        cmd = f"python3 -c \"import market_data; print('CRYPTO:', market_data.get_crypto_prices({sym_json})); print('FG:', market_data.get_fear_greed()); print('SPY:', market_data.get_stock_quote('SPY'))\""
        output = _run_fleet(cmd, timeout=30)
        return {
            'output': output,
            'symbols_requested': sym_list,
            '_note': 'SIMULATED context — market data is real but used only for paper trading'
        }

    @mcp.tool()
    def meridian_reset(balance: float = 10000.0) -> dict:
        """
        Reset the SIMULATED paper trading portfolio to starting balance.
        Clears all positions and trade history. Starting fresh.
        balance: starting cash amount in USD (default $10,000).
        SIMULATED ONLY — no real money involved.
        """
        assert REAL_TRADING == False, 'Safety check: real trading is disabled'
        if balance < 100 or balance > 1000000:
            return {'status': 'rejected', 'reason': 'Balance must be between $100 and $1,000,000', '_note': 'SIMULATED'}
        cmd = f'python3 meridian.py reset --balance {balance}'
        output = _run_fleet(cmd)
        return {
            'status': 'reset',
            'starting_balance': balance,
            'output': output,
            '_note': 'SIMULATED — portfolio reset complete'
        }
