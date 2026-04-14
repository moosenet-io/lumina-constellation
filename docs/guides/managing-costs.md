# Managing Inference Costs

Lumina targets ~$0.08/day for routine operations.

## The inference pyramid

```
                    Opus (gated)         Critical decisions only
                  Sonnet/Haiku           On-demand: research, reasoning
               Local Qwen ($0)           Parsing, formatting, fallback
          Python + Templates ($0)        Default: 90% of all operations
```

## What costs money

| Operation | Cost | Provider |
|-----------|------|---------|
| Morning briefing synthesis | $0 | Qwen local |
| Seer light research | $0 | Qwen local |
| Seer deep research | $2-5 | Sonnet (on-demand) |
| Obsidian Circle consultation | $0.30-1.00 | Mixed (OpenRouter) |
| Vector dev loop | $0.20 | Sonnet |
| the operator's Matrix conversation | Variable | Sonnet reactive |

## Template library

Instead of LLM calls, Lumina uses YAML templates for:
- Health coaching messages (`vitals_coaching.yaml`)
- Budget alerts (`ledger_alerts.yaml`)
- Trading alerts (`meridian_alerts.yaml`)
- Renewal reminders (`relay_reminders.yaml`)

Templates live at `/opt/lumina-fleet/shared/templates/`.

## Monitoring

Ask Lumina: "How much did I spend today?" → Myelin responds with breakdown.
Check Soma → Cost dashboard for weekly trends.
