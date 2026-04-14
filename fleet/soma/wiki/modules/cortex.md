# Cortex — Code Intelligence

Cortex provides code intelligence for Lumina Constellation. It performs AST analysis, blast radius detection, code review with certificates, and audit reports. Optionally routes to the Obsidian Circle council for multi-model review.

**Deploys to:** CT310 at `/opt/lumina-fleet/cortex/`
**Inference cost:** Cloud Sonnet (~$0.01–0.05 per review)
**Key dependency:** [code-review-graph](https://github.com/tirth8205/code-review-graph) v2.2.2

## What Cortex Does

1. **Blast radius analysis** — Given a file or function change, identify all downstream dependents using tree-sitter AST parsing
2. **Code review** — Generate a structured review with findings, risk level, and a review certificate
3. **Audit report** — Full codebase audit: complexity, test coverage, dependency risks
4. **Council review** — For high-risk changes, convene the Obsidian Circle (4 models) for independent assessment

## Review Certificate

Cortex issues a certificate for each completed review:

```
CORTEX REVIEW CERTIFICATE
--------------------------
File:    terminus/nexus_tools.py
Commit:  a3f8c2d
Risk:    LOW
Verdict: APPROVED
Signed:  cortex-v1 / 2026-04-13T09:15:00Z
```

Certificates are stored in Engram and referenced in commit messages.

## Blast Radius Analysis

Uses tree-sitter to build an AST knowledge graph, then calculates which functions/modules depend on the changed code:

```python
# Example: changing nexus_tools.py::nexus_send()
# Blast radius includes:
#   - axon.py (imports nexus_send)
#   - vigil/briefing.py (imports nexus_send)
#   - Any test files
```

Risk levels: `LOW` (<5 dependents), `MEDIUM` (5–20), `HIGH` (>20 or core infrastructure)

## MCP Tools (in Terminus)

| Tool | Description |
|------|-------------|
| `cortex_blast_radius(file, function)` | Calculate downstream impact of a change |
| `cortex_review(file, diff)` | Generate a structured code review |
| `cortex_audit(path)` | Full codebase audit report |
| `cortex_council(file, question)` | Route to Obsidian Circle for multi-model review |

## Obsidian Circle Integration

For `risk: HIGH` reviews, Cortex can convene the Obsidian Circle (Mr. Wizard):
- Claude (Anthropic)
- GPT-4 (OpenAI)
- Gemini (Google)
- DeepSeek (independent)

Each model reviews independently. Disagreements are surfaced in the report. The certificate is only issued when all models approve, or with a noted dissent.

## Files

| File | Purpose |
|------|---------|
| `cortex.py` | Main agent. AST analysis, review generation, report output. |
| `cortex.service` | systemd service unit. |

## HTML Reports

Cortex generates HTML audit reports using `report_template.py`:

```python
from soma.report_template import Report

r = Report(title='Cortex Audit — terminus/', module='cortex',
           metadata={'risk': 'LOW', 'files': '20', 'cost': '$0.03'})
r.add_kpi('Files reviewed', '20', style='success')
r.add_kpi('High risk', '0', style='success')
r.add_section('Findings', r.table(...))
path = r.save()
```

## Related

- [Architecture Overview](../architecture/constellation-overview.md)
- [Inference De-Bloating](../architecture/inference-de-bloating.md) — Sonnet tier justification
- code-review-graph: [github.com/tirth8205/code-review-graph](https://github.com/tirth8205/code-review-graph)
