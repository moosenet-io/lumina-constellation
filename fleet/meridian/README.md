# ✦ Meridian

> "Paper money. Real lessons. Zero risk."

**Meridian** is Lumina's paper trading sandbox — it manages a virtual portfolio using real market data, tracks reasoning behind every decision in a journal, and benchmarks portfolio performance against buy-and-hold. No real money, ever.

## What it does

- **Virtual portfolio management**: buy/sell positions with simulated execution
- **Real market data**: price feeds via AlphaVantage and Finnhub APIs
- **Decision journal**: every trade logged with the LLM's reasoning — reviewable via Soma
- **Performance tracking**: compares portfolio vs. S&P 500 and buy-and-hold benchmarks
- **Weekly reports**: Sunday evening portfolio summary delivered via Matrix

## Key files

| File | Purpose |
|------|---------|
| `meridian.py` | Portfolio management, trade execution, position tracking |
| `market_data.py` | Real-time and historical price data fetching |
| `portfolio.py` | Portfolio state, P&L calculation, benchmark comparison |

## Talks to

- **Terminus** (`meridian_tools.py`) — MCP tools expose portfolio operations to IronClaw
- **Engram** — stores trade journal entries and portfolio snapshots
- **Soma** — portfolio dashboard and performance charts
- **Nexus** — routes weekly reports and significant trade alerts

## Configuration

```bash
ALPHAVANTAGE_API_KEY=...
FINNHUB_API_KEY=...
LITELLM_API_KEY=your-virtual-key
```

All trades are **simulated only**. Meridian never connects to a brokerage or real trading account.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
</content>
</invoke>