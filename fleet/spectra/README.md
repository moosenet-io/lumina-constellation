# ✦ Spectra

> Lumina's eyes on the web.

**Spectra** is the browser automation and visual capture module that allows Lumina to "see" and interact with the web.

## What it does

- Automates web browsing using Playwright and Chromium.
- Captures screenshots of dashboards and websites for the morning briefing.
- Performs browser-based tasks like filling forms or clicking interactive elements.
- Sanitizes HTML for LLM consumption, stripping scripts and ads.
- Operates as a background worker for other agents requiring web access.

## Key files

| File | Purpose |
|------|---------|
| `spectra_worker.py` | Playwright-based browser task execution |
| `spectra_service.py` | Flask-based API for browser automation |
| `spectra_sanitizer.py` | Extracts clean text and structure from raw HTML |
| `spectra_config.yaml` | Browser viewport and user-agent settings |

## Talks to

- **[Seer](../seer/)** — Acts as the execution layer for web research.
- **[Vigil](../vigil/)** — Provides screenshots for the daily morning report.
- **[Soma](../soma/)** — Feeds live dashboard views to mission control.

## Configuration

Runs in a containerized Playwright environment. Browser settings defined in `spectra_config.yaml`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
