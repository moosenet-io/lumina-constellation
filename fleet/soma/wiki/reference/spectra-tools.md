# Spectra MCP Tool Reference

All tools require a valid `consumer_key` (MY.1–MY.9). See [Spectra Overview](../modules/spectra.md) for the key table.

Default key: `MY.2` (Lumina orchestrator). Override per-call for other consumers.

Set `SPECTRA_URL` env var on Terminus to the fleet host port 8084.

---

## Navigation

### `spectra_navigate(url, consumer_key, session_id?, headed?)`
Navigate browser to URL. Returns `{ok, session_id, title, url}`.

- `session_id`: pass existing session to continue in same browser context
- `headed=True`: browser visible in Soma Live View

### `spectra_click(session_id, selector, consumer_key)`
Click element by CSS selector. Raises 500 if element not found.

### `spectra_type(session_id, selector, text, consumer_key)`
Fill input by CSS selector.

### `spectra_fill_form(session_id, fields, consumer_key)`
Fill multiple fields: `fields = {'#email': 'user@example.com', '#password': '...'}`.

### `spectra_wait_for(session_id, selector?, state?, timeout_ms?, consumer_key)`
Wait for selector to reach `state` (`visible`/`hidden`/`attached`/`detached`) or for network idle (omit selector).

### `spectra_execute_js(session_id, script, consumer_key)`
Execute JavaScript. Return value included in response. **Audit-logged always.**

---

## Content Extraction

### `spectra_extract_text(session_id, consumer_key)`
Extract + sanitize page text. Returns `{ok, text, flags}`.
- `text`: wrapped in `[UNTRUSTED_WEB_CONTENT]` delimiters
- `flags`: list of sanitization events (`hidden_element_stripped`, `zero_width_chars_removed`, etc.)
- Max output: ~2000 tokens (~8000 chars)

### `spectra_extract_links(session_id, consumer_key)`
Returns `{ok, links: [{href, text}], count}`.

### `spectra_accessibility_snapshot(session_id, consumer_key)`
Returns Playwright accessibility tree as `{ok, snapshot: {role, name, children, ...}}`.
**20–50× more token-efficient than screenshot for LLM page analysis.**

### `spectra_pdf(session_id, consumer_key)`
Returns `{ok, pdf_b64}` (base64-encoded PDF).

---

## Screenshots

### `spectra_screenshot(session_id, consumer_key)`
Returns `{ok, png_b64}` (base64 PNG, current viewport).

### `spectra_internal_screenshot(url, consumer_key)`
Screenshot using `spectra-internal` container (LAN-only, no internet). URL must be on allowlist.

### `spectra_visual_diff(screenshot_b64_before, screenshot_b64_after, page?, consumer_key)`
Compare two screenshots. Returns:
- `diff_score`: 0–100 (0 = identical, 100 = completely different)
- `structural_score`: perceptual hash similarity
- `diff_image_b64`: PNG with changed regions highlighted in red
- `changed_regions`: list of bounding boxes
- `engram_fact_id`: stored in Engram with `source='spectra-diff'`

---

## Storage

### `spectra_store_content(session_id, consumer_key, store_screenshot?, store_accessibility?, store_links?)`
Extract page content, sanitize, store to Engram with `source='spectra'`. Returns `{ok, url, title, text, screenshot_b64, accessibility, links}`.

Content is deduped by URL + date. Same URL same day = update.

---

## Session Management

### `spectra_session_list(consumer_key)`
Lists active sessions: `{ok, sessions: [{id, consumer, url, state, pages, started}]}`.
Automatically closes sessions idle > 15 minutes.

### `spectra_session_close(session_id, consumer_key)`
Close session and save rrweb recording. Returns `{ok}`.

### `spectra_session_recordings(consumer_key)`
List recordings: `{ok, count, recordings: [{session_id, size_bytes, modified}]}`.

---

## Observability

### `spectra_live_view_url(consumer_key)`
Returns Soma URL for Live View. Open in browser to watch live.

### `spectra_audit_query(filter_key?, action?, limit?, consumer_key)`
Query `audit.jsonl`. Filter by consumer key and/or action type.
Actions: `navigate`, `screenshot`, `extract_text`, `accessibility_snapshot`, `store_content`, `execute_js`, `hitl_request`, `internal_screenshot`.

---

## HITL + Diagnostics

### `spectra_request_human_help(session_id, reason, consumer_key)`
Trigger HITL pipeline. Sets session state to `waiting_human`. Captures screenshot.
Reason values: `login_form_detected`, `captcha_detected:NAME`, `ambiguous_content`, `manual`.

### `spectra_self_test(consumer_key?)`
Run 6-step self-test. Returns `{ok, steps, passed, total}`.
Requires `consumer_key=MY.1` (operator). Posts results to Sentinel.

---

## Error Codes

| Code | Meaning |
|------|---------|
| 403 | Invalid key, disabled key, or key not in config |
| 404 | Session not found |
| 429 | Daily budget exhausted, or per-second rate limit exceeded (includes `Retry-After` header) |
| 500 | Browser error (navigate timeout, element not found, etc.) |
