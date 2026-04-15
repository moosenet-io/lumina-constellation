# ✦ Seer

> "Research that reads the whole internet so you don't have to."

**Seer** is Lumina's web research agent — it decomposes complex queries, searches across multiple sources via SearXNG, extracts content with prompt injection defense, and synthesizes findings into a structured report.

## What it does

- **Query decomposition**: breaks complex research questions into targeted sub-queries
- **Multi-source search**: queries SearXNG (self-hosted, privacy-preserving meta-search)
- **Content extraction**: sanitizes web content before passing to LLM (blocks script injection)
- **Report synthesis**: combines sources into structured markdown with citations
- **Spectra integration**: uses Seer via `spectra_navigate` for JS-heavy pages that SearXNG can't index

## Key files

| File | Purpose |
|------|---------|
| `seer.py` | Main research agent — query decomposition, search, synthesis |
| `sanitizer.py` | Content sanitization before LLM ingestion |

## Talks to

- **Terminus** (`seer_tools.py`) — MCP tools expose Seer to IronClaw
- **Engram** — stores research findings for RAG retrieval by other agents
- **Spectra** — browser-based content extraction for JS-heavy sources
- **Obsidian Circle** — council members can reference past Seer research via Engram

## Configuration

```bash
SEARXNG_URL=http://your-searxng-host:8080
LITELLM_URL=http://your-litellm-host:4000
LITELLM_API_KEY=your-virtual-key   # Seer uses MY.6 (seer-research, $2/day budget)
```

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
