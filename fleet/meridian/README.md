# ✦ Meridian

> Paper money. Real lessons. Zero risk.

**Meridian** is the paper trading sandbox and financial intelligence module for Lumina Constellation.

## What it does

- Simulates equity and cryptocurrency trading in a zero-risk paper money environment.
- Tracks market sentiment, technical indicators, and Fear & Greed indices.
- Maintains a virtual portfolio and logs all agent-led trading decisions.
- Performs performance analysis and generates risk/reward reports.
- Provides financial market context to the Obsidian Circle for economic deliberations.

## Key files

| File | Purpose |
|------|---------|
| `meridian.py` | Main paper trading orchestration |
| `portfolio.py` | Portfolio management and trade execution logic |
| `market_data.py` | Fetches quotes, sentiment, and indicators |
| `README.md` | This documentation |

## Talks to

- **[Engram](../engram/)** — Stores portfolio history and decision logs.
- **[Vigil](../vigil/)** — Provides financial summaries for the morning briefing.
- **[Myelin](../myelin/)** — Reports on virtual P&L for "wealth building" simulations.

## Configuration

Market data providers (e.g., Alpha Vantage, CoinGecko) configured in `meridian.py`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
