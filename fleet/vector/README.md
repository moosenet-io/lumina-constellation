# ✦ Vector

> Ship it. Test it. Ship it again.

**Vector** is the development lifecycle module that manages code changes, testing, and deployment loops.

## What it does

- Automates the "Plan-Act-Validate" development cycle.
- Integrates with Calx for running unit tests and integration suites.
- Manages agent-led refactoring and feature implementation tasks.
- Enforces code quality guardrails before staging changes.
- Maintains a local vector index of the codebase for RAG-assisted coding.

## Key files

| File | Purpose |
|------|---------|
| `vector.py` | Main orchestration for development loops |
| `calx/` | Test runner and execution environment |
| `guardrails.py` | Quality and safety checks for code changes |
| `council_gate.py` | Multi-model review gate for complex PRs |

## Talks to

- **[Cortex](../cortex/)** — Retrieves architectural insights and dependency maps.
- **[Engram](../engram/)** — Stores and queries codebase patterns.
- **[Terminus](../../terminus/)** — Uses Gitea and GitHub tools for version control.

## Configuration

Requires path to the local repository. Guardrail strictness configured in `config/`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
