---
name: code-review
description: Semi-formal code review using AST analysis, blast radius calculation, and multi-model council
version: 1.0
author: Peter Boose
license: MIT
agent: cortex
container: <fleet-host>
tags: [code, review, ast, cortex, quality]
---

# Code Review

Perform semi-formal code review on a target file or pull request using Cortex's code intelligence tools.

## Procedure

1. cortex_scope: Identify all callers and dependencies of changed functions (blast radius)
2. cortex_analyze: Run code-review-graph AST analysis, extract function signatures, complexity
3. Identify risk level: Low (utility functions), Medium (shared modules), High (API surfaces)
4. For High-risk changes: invoke Obsidian Circle council (wizard_consult) for multi-model review
5. cortex_certify: Generate review certificate with findings and recommendations
6. Post results to Nexus for Lumina to relay to the operator

## Inference de-bloat

- Steps 1-2: Python AST analysis via code-review-graph ($0)
- Step 3: Python threshold rules ($0)
- Step 4: Only for High risk — cloud inference (~$0.05 per review)
- Step 5: Python template ($0)

## Prerequisites

code-review-graph must be installed: pip install code-review-graph
