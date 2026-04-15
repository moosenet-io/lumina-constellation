# Vigil — Briefing Agent

Vigil produces morning and evening briefings for Lumina Constellation users. It aggregates data from multiple sources, formats a summary, and delivers it via Matrix or HTML dashboard — without making unnecessary LLM calls.

**Deploys to:** <fleet-host> at `/opt/lumina-fleet/vigil/`
**Trigger:** IronClaw routine (default 07:00 and 17:00)
**Inference cost:** Low — data assembly is Python; narrative synthesis uses local Qwen only when needed.

## What Vigil Does

1. Fetches data from all configured sources in parallel
2. Assembles a structured summary using Python templates
3. Generates an HTML dashboard (`briefing_dashboard.py`) using `constellation.css`
4. Sends the briefing text to the Matrix channel via Lumina
5. Writes the HTML to a known path for the web dashboard to serve

## Data Sources

| Source | What it provides | Method |
|--------|-----------------|--------|
| **NewsAPI** | Top headlines by category | REST API |
| **GNews** | Secondary news source | REST API |
| **TomTom** | Commute time vs. baseline | REST API |
| **Google Calendar** | Today's events via CalDAV | CalDAV (App Password) |
| **Google IMAP** | Unread email count, flagged items | IMAP (App Password) |
| **Weather API** | Forecast, temperature, conditions | REST API |
| **Grocy** | Pantry items expiring today | REST API |
| **Actual Budget** | Current budget status | REST API |

## Files

| File | Purpose |
|------|---------|
| `briefing.py` | Main agent script. Data fetch, template assembly, Matrix delivery. |
| `briefing_dashboard.py` | HTML report generator. Uses `constellation.css` for styling. |

## Triggering Vigil

Vigil is triggered by an IronClaw routine configured in LUMINA.md. It can also be triggered on-demand:

```bash
# From <fleet-host>
python3 /opt/lumina-fleet/vigil/briefing.py --test
```

Or via Lumina (IronClaw MCP tool):
```
vigil_run(type="morning")
```

## MCP Tools (in Terminus)

| Tool | Description |
|------|-------------|
| `vigil_run(type)` | Trigger a briefing (morning or evening) |
| `vigil_dashboard()` | Return URL to current briefing HTML dashboard |

## Design Constraints

- All data aggregation is Python — no LLM calls for fetching or formatting
- HTML reports use `constellation.css` exclusively — no inline styles
- Local Qwen (via Ollama) is used only for paragraph-level narrative synthesis if configured
- Commute alerts fire separately if delay exceeds 25% above baseline (threshold check, no inference)

## Configuration

```yaml
# constellation.yaml — vigil section
vigil:
  schedule_morning: "0 7 * * *"
  schedule_evening: "0 17 * * *"
  location: "Ottawa, ON"
  commute_baseline_minutes: 28
  news_categories: [business, technology, sports]
  google_caldav_url: "https://caldav.google.com/..."
```

## Related

- [Fleet Overview](../architecture/constellation-overview.md)
- MCP tools: `terminus/vigil_tools.py`
- Design system: `fleet/shared/constellation.css`
- Template library: `fleet/shared/templates/`
