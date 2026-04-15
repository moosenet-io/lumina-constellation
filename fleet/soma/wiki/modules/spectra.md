# Spectra — Browser Agent

Spectra is Lumina's browser automation module — eyes on the web. It deploys a sandboxed Playwright instance, exposes browser capabilities through Terminus MCP tools, provides observability via Soma (embedded Live View, session recording playback), human-in-the-loop handoff via Matrix, and persistent content storage integrated with Engram for RAG retrieval.

## Architecture

```
IronClaw → Terminus MCP → spectra_tools.py → HTTP :8084 → spectra-service → Chromium → internet
                                                        ↘ HTTP :8085 → spectra-internal → LAN only
Soma /spectra → WebSocket proxy → noVNC (Docker-internal) → spectra Xvfb display
Spectra → Engram (spectra: namespace) → all agents via engram_query(source='spectra')
```

## Containers

| Container | Port | Role |
|-----------|------|------|
| `spectra` | 8084 | Main browser (internet + sanitization) |
| `spectra-internal` | 8085 | LAN-only browser (no internet) |
| `spectra-novnc` | internal | noVNC Live View (accessed via Soma proxy) |

## 21 MCP Tools

| Tool | Description |
|------|-------------|
| `spectra_navigate` | Navigate to URL, return title + session_id |
| `spectra_screenshot` | Capture page as base64 PNG |
| `spectra_click` | Click element by CSS selector |
| `spectra_type` | Fill input field |
| `spectra_extract_text` | Extract + sanitize page text (10-stage pipeline) |
| `spectra_extract_links` | Extract all hyperlinks + anchor text |
| `spectra_fill_form` | Fill multiple form fields |
| `spectra_execute_js` | Execute JavaScript (audit-logged) |
| `spectra_pdf` | Save page as PDF |
| `spectra_wait_for` | Wait for selector or network idle |
| `spectra_session_list` | List active sessions |
| `spectra_session_close` | Close session, save recording |
| `spectra_live_view_url` | Get Soma Live View URL |
| `spectra_session_recordings` | List rrweb session recordings |
| `spectra_request_human_help` | Trigger HITL pipeline |
| `spectra_store_content` | Extract + store to Engram |
| `spectra_internal_screenshot` | Screenshot LAN services (allowlisted) |
| `spectra_audit_query` | Query audit log |
| `spectra_accessibility_snapshot` | Playwright accessibility tree as JSON |
| `spectra_visual_diff` | Compare screenshots (Pillow + imagehash) |
| `spectra_self_test` | Run 6-step self-test |

## Access Control

Spectra uses consumer keys (MY.1–MY.9) for access control. Keys are configured in `spectra_config.yaml`:

| Key | Consumer | Budget/Day | Rate/s |
|-----|----------|-----------|--------|
| MY.1 | Peter (operator) | Unlimited | 50 |
| MY.2 | Lumina | 200 | 5 |
| MY.3 | IronClaw (disabled) | 0 | — |
| MY.4 | Vigil | 20 | 2 |
| MY.5 | Sentinel | 10 | 2 |
| MY.6 | Seer | 50 | 5 |
| MY.7 | Vector | 30 | 5 |

Config hot-reloads within ~5 seconds — no container restart needed.

## Soma Pages

- `/spectra` — Live View + active sessions + usage
- `/spectra/recordings` — rrweb session replay
- `/spectra/config` — Consumer access table + audit log viewer

## Engram Integration

All extracted content is stored with `source='spectra'` and queryable by all agents:

```python
engram_query(source='spectra', query='topic')           # Any web content
engram_query(source='spectra-feedback', query='nav')    # UX feedback
engram_query(source='spectra-diff', query='/')          # Visual diffs
engram_query(source='spectra-snapshot', query='page')   # Accessibility snapshots
```

Deduplication: same URL + same day = update (not duplicate).
Retention: text 90 days, screenshots/recordings 7 days (thumbnails retained).
