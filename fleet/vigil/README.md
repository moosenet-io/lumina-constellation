# Vigil — Briefing Agent

Vigil produces morning and evening briefings for Lumina Constellation users. It aggregates data from multiple sources, formats a summary, and delivers it via Matrix or HTML dashboard — without making unnecessary LLM calls.

**Deploys to:** <fleet-host> (<fleet-server-ip>) at `/opt/lumina-fleet/vigil/`
**Trigger:** IronClaw routine (configurable schedule — default 07:00 and 17:00)
**Inference cost:** Low. Data assembly is Python; narrative synthesis uses a local model only when needed.

---

## What Vigil Does

1. Fetches data from all configured sources in parallel.
2. Assembles a structured summary using Python templates.
3. Generates an HTML dashboard (`briefing_dashboard.py`) using `constellation.css`.
4. Sends the briefing text to the Matrix channel via Lumina.
5. Writes the HTML to a known path for the web dashboard to serve.

---

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

---

## Files

| File | Purpose |
|------|---------|
| `briefing.py` | Main agent script. Data fetch, template assembly, Matrix delivery. |
| `briefing_dashboard.py` | HTML report generator. Uses `constellation.css` for styling. |

---

## MCP Tools (via Terminus)

| Tool | Description | Parameters |
|------|-------------|------------|
| `vigil_run` | Trigger a briefing immediately | `schedule: morning\|evening` |
| `vigil_dashboard` | Return URL of the latest briefing HTML | — |
| `vigil_status` | Return last briefing timestamp and status | — |

These are defined in `terminus/vigil_tools.py`.

---

## Design Constraints

- All data aggregation is Python — no LLM calls for fetching or formatting.
- The HTML report uses `constellation.css` exclusively. No inline styles.
- Local model (Qwen via Ollama) is used only for paragraph-level narrative synthesis if configured. Can be disabled for pure-template mode.
- Commute alerts fire separately from the briefing if the delay exceeds 25% above baseline (threshold check, no inference).

---

## Architecture

- **Runs on:** <fleet-host> (`<fleet-server-ip>`) at `/opt/lumina-fleet/vigil/`
- **Dependencies:** Python 3.11+, `requests`, `caldav`, `imaplib` (stdlib), `jinja2`
- **Connections:** NewsAPI, GNews, TomTom, Google CalDAV, Google IMAP, weather API, Grocy (self-hosted), Actual Budget (self-hosted); output via Nexus → Lumina → Matrix

## Configuration

| Variable | Purpose | Default |
|----------|---------|---------|
| `NEWS_API_KEY` | NewsAPI key | — |
| `GNEWS_API_KEY` | GNews key | — |
| `TOMTOM_API_KEY` | TomTom routing key | — |
| `GOOGLE_EMAIL` | Google account email | — |
| `GOOGLE_APP_PASSWORD` | App Password for CalDAV + IMAP | — |
| `WEATHER_API_KEY` | Weather provider key | — |
| `GROCY_BASE_URL` | Grocy server URL | — |
| `GROCY_API_KEY` | Grocy API key | — |
| `ACTUAL_BASE_URL` | Actual Budget server URL | — |
| `ACTUAL_API_KEY` | Actual Budget API key | — |
| `BRIEFING_HTML_PATH` | Where to write the dashboard HTML | `/var/www/html/briefing.html` |
| `VIGIL_LLM_ENABLED` | Enable local model narrative synthesis | `false` |

## History / Lineage

Vigil descends from "Agent Briefly" (`moosenet/agent-briefly`), renamed in session 11 as part of the Lumina naming consolidation. The briefing script was first built in session 3; the HTML dashboard (`briefing_dashboard.py`) was added in session 8 using `constellation.css`. Commute alerting was separated from the main briefing in session 7 to reduce noise.

## Credits

- CalDAV integration — [python-caldav](https://github.com/python-caldav/caldav) v3.1.0 (GPL/Apache, python-caldav contributors)
- Design system — `constellation.css` (see `fleet/shared/`)
- Template library — `templates/vigil_notifications.yaml`

## Related

- [fleet/README.md](../README.md) — Fleet overview
- [terminus/vigil_tools.py](../../terminus/vigil_tools.py) — MCP tools for triggering Vigil from Lumina
- [fleet/shared/constellation.css](../shared/constellation.css) — Shared design system
- [fleet/shared/templates/](../shared/templates/) — Template library for briefing messages
