# Cortex — Code Intelligence (Module 15)

Cortex wraps [code-review-graph](https://github.com/tirth8205/code-review-graph) (MIT, by Tirth Patel) for Tree-sitter AST analysis.

## What it does

- **Blast radius**: Given changed files, returns all affected files and callers
- **Risk score**: 0-10 risk assessment for a set of changes
- **External audit**: Clone → analyze → HTML report → sandbox cleanup
- **Vector integration**: Pre-flight scoping + post-flight risk check

## Blast radius in practice

When Vector edits `axon/axon.py`, Cortex tells it:
- Blast radius: [axon.py, axon_tools.py] (2 files)
- Token reduction: 97.1% (only check 2 files, not all 47)
- Risk score: 2/10

## High-risk files (lumina-fleet)

Files with most dependents — change carefully:
- `vector/backends/interfaces.py` — 7 dependents
- `naming.py` — 6 dependents (agents use this for display names)

## External audit

```
cortex_audit(url='https://github.com/owner/repo')
```
Returns HTML report at http://<fleet-server-ip>/code/{report-name}.html
