# Inference De-Bloating

**Target: $0.08/day for routine operations. Every LLM call must be justified.**

The most important architectural principle in Lumina Constellation. Most "AI tasks" don't need AI. A Python script checking disk usage is faster, cheaper, and more reliable than asking an LLM to do it.

## The Decision Chain

Stop at the first YES and use that tier. Work your way down only if the tier above can't handle it.

| # | Can this be done by... | Cost | Use when |
|---|----------------------|------|---------|
| 1 | Python standard library | **$0** | Math, JSON parsing, HTTP status checks, SQL queries, file I/O |
| 2 | Template + variables | **$0** | Notifications, alerts, formatted summaries with known structure |
| 3 | Keyword lookup table | **$0** | Routing, classification with finite known categories |
| 4 | Local Qwen (Ollama) | **$0** | NL parsing that regex can't do, short text generation |
| 5 | Local Qwen (generation) | **$0** | NL text beyond templates but doesn't need cloud quality |
| 6 | Cloud Haiku | **~$0.001** | Multi-source synthesis requiring some reasoning |
| 7 | Cloud Sonnet | **~$0.01–0.05** | Complex reasoning, research synthesis, code review |
| 8 | Cloud Opus (gated) | **Subscription** | Critical architectural decisions only |

## What Python Handles (Not LLM)

These patterns should never reach an LLM:

- HTTP status code checks, JSON parsing, SQL queries, result formatting
- Math: averages, percentages, deltas, comparisons, thresholds
- Set operations: ingredient matching, overlap calculation, deduplication
- Threshold alerts: budget > 80%, commute > 25% delta
- Date/time arithmetic: next due date, streak counting, days remaining
- File I/O: read config, write HTML from template
- API call + response forwarding (Grocy, LubeLogger, Actual, TomTom)
- Sorting, ranking, filtering by numeric fields

**Example — Sentinel health check:**

```python
# WRONG: send disk usage to LLM to determine if it's a problem
# RIGHT: Python threshold check
def check_disk(path: str, threshold: int = 85) -> CheckResult:
    stat = os.statvfs(path)
    pct = int((1 - stat.f_bavail / stat.f_blocks) * 100)
    return CheckResult(ok=pct < threshold, name=path, detail=f"{pct}% used")
```

## What Templates Handle (Not LLM)

These message types use pre-written templates from `/opt/lumina-fleet/shared/templates/`:

- Health coaching messages (celebrations, nudges, pattern observations)
- Budget alerts (50%, 80%, 100% threshold messages)
- Trading alerts (price spike/drop, portfolio summary)
- Dashboard insights (delta up/down, milestones, tips rotation)
- Matrix notifications (briefing ready, system alert)
- Commute alerts (traffic worse than baseline)
- Renewal/maintenance reminders (document expiry, service due)

**Example — Budget alert template:**

```yaml
# templates/ledger.yaml
budget_alert_80:
  text: "Budget alert: {category} is at {pct}% of monthly limit ({spent} / {limit})."
  channel: matrix
  priority: urgent
```

```python
# WRONG: call Haiku to write a budget alert message
# RIGHT: fill the template
msg = templates['budget_alert_80'].format(
    category='Groceries', pct=83, spent='$332', limit='$400'
)
```

## Where LLM Is Legitimately Needed

- Briefing synthesis (combining 8+ data sources into narrative)
- Research reports (Seer multi-source analysis)
- Conversational responses to the operator's Matrix messages
- Architectural consultation (Obsidian Circle)
- Trade reasoning (Meridian decision journal)
- Code review synthesis (Cortex + council)
- Ambiguous NL parsing that regex/keywords can't handle

## Cost Breakdown

| Tier | Frequency | Daily Cost |
|------|-----------|-----------|
| Python / templates / SQL | ~90% of operations | $0.00 |
| Local Ollama models | ~8% of operations | $0.00 |
| Cloud Haiku / small models | ~2% of operations | ~$0.10–0.30 |
| Cloud Sonnet | Reasoning tasks only | ~$0.20–0.50 |
| Cloud Opus | Gated — architecture only | Subscription |
| **Total daily target** | | **under $1.00** |

## Enforcement Pattern

Every agent module should document its inference tier:

```python
# sentinel/ops.py
# INFERENCE TIER: Pure Python — $0
# No LLM calls. All health checks are threshold comparisons.
```

```python
# vigil/briefing.py
# INFERENCE TIER: Python (data fetch) + local Qwen (narrative synthesis)
# LLM called once per briefing, local model only
```

Myelin watches actual costs. If a module exceeds its expected tier, Myelin flags it for review.

## Related

- [Architecture Overview](constellation-overview.md) — How de-bloating fits the system
- [Myelin module](../modules/myelin.md) — Cost governance and runaway detection
- Template library: `/opt/lumina-fleet/shared/templates/`
