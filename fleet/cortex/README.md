# ✦ Cortex

> Your code has opinions about itself. Cortex reads them.

**Cortex** is the code intelligence module that provides deep architectural analysis of the Lumina codebase.

## What it does

- Builds and maintains a dependency graph of all constellation modules.
- Analyzes repository structure to identify risks and technical debt.
- Detects architectural "communities" and module clusters.
- Provides risk scores for proposed changes during dev loops.
- Feeds codebase metadata to agents for better context during coding tasks.

## Key files

| File | Purpose |
|------|---------|
| `cortex.py` | Main entry point for repository analysis |
| `README.md` | This documentation |

## Talks to

- **[Vector](../vector/)** — Provides risk signals and dependency context for PRs.
- **[Engram](../engram/)** — Stores architectural patterns and graph metadata.
- **[Terminus](../../terminus/)** — Uses Gitea tools to fetch repository history.

## Configuration

Requires path to the local repository. Uses `code_review_graph` for incremental updates.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
