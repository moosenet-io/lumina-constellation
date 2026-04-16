# ✦ Seer

> Research that reads the whole internet so you don't have to.

**Seer** is the web research module that performs deep, autonomous investigation across the public internet.

## What it does

- Conducts multi-step research queries using search engines and direct site crawling.
- Summarizes high-volume web content into structured briefings.
- Sanitizes and filters web data to remove noise and irrelevant information.
- Verifies facts and cross-references information from multiple sources.
- Integrates findings directly into the constellation's memory (Engram).

## Key files

| File | Purpose |
|------|---------|
| `seer.py` | Main autonomous research orchestration |
| `sanitizer.py` | Cleans and formats raw web data for LLM consumption |
| `config/` | Search engine API keys and crawl parameters |

## Talks to

- **[Spectra](../spectra/)** — Uses the browser module for advanced site navigation.
- **[Engram](../engram/)** — Stores researched facts and source citations.
- **[Synapse](../synapse/)** — Delivers urgent research findings to the operator.

## Configuration

Requires API keys for search providers (e.g., Tavily, Perplexity, or Google) in the environment.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
