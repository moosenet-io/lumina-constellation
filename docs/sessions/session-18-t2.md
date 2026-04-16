# Session 18 T2 — Soma Visual Feedback Loop

**Date:** 2026-04-16
**Commit:** `88ffd19`
**Scope:** BA.19 (feedback engine) + BA.20 (UX improvements)

---

## What was built

### BA.19 — `fleet/spectra/spectra_soma_feedback.py`

Soma visual feedback loop engine. Navigates all 11 Soma admin pages via the
`spectra-internal` browser container, takes accessibility snapshots and screenshots,
sends them to Lumina Fast (Qwen local) for UX analysis, and stores results in Engram
and `/data/spectra/feedback/`.

**Run it on CT310:**
```bash
cd /opt/spectra
export SPECTRA_INTERNAL_URL=http://192.168.0.120:8085
export LITELLM_URL=http://192.168.0.215:4000
export LITELLM_MASTER_KEY=sk-Twhg4AdqhSdseMxuhwi_EA
export SOMA_JWT_SECRET=soma-moosenet-2026
export FLEET_DIR=/opt/lumina-fleet
python3 spectra_soma_feedback.py
# or: python3 spectra_soma_feedback.py --page status config security
```

**Output:**
- `/data/spectra/feedback/{page}.png` — screenshot per page
- `/data/spectra/feedback/{page}.json` — findings JSON per page
- `/data/spectra/feedback/feedback_summary.json` — aggregate (pages, severity counts, all findings)
- Engram KB — findings stored under `soma-feedback/{page}/{category}/{n}` namespace

**Results from first run:**
- 11 pages analysed (status, config, security, skills, plugins, sessions, logs, vector, synapse, spectra, council)
- 40 findings: 0 critical, 11 high, 29 medium
- Pages with LLM failures due to large accessibility trees: plugins, vector, synapse (0 findings; re-runnable)

---

### BA.20 — 5 UX improvements implemented

Based on BA.19 findings, top 5 high-severity issues fixed:

| # | File | Change |
|---|------|--------|
| 1 | `fleet/soma/templates/status.html` | `⛔ CRITICAL` badge enlarged to 1.1rem + CSS pulse animation (`pulse-error`) |
| 2 | `fleet/soma/templates/status.html` | `alert-error` banner appears when status ≠ ok, with operator guidance |
| 3 | `fleet/soma/templates/status_grid.html` | Retry buttons labeled by service: "↺ Retry IronClaw", "↺ Retry Nexus", etc.; long error messages truncated to 60 chars with full text in `title` tooltip |
| 4 | `fleet/soma/templates/security.html` | `sec-summary-badge` in page header shows live secret health ("All 8 secrets OK" / "2 expiring soon") |
| 5 | `fleet/soma/templates/logs.html` | `colorizeLog()` JS function applies color coding after HTMX loads: ERROR=red, WARN=yellow, INFO=secondary, DEBUG=tertiary |

---

## Infrastructure fixes (required to make BA.19 work)

### 1. htmx CDN → LAN Apache

Soma templates previously loaded htmx from `https://unpkg.com/htmx.org@1.9.10`.
`spectra-internal` iptables blocks all internet traffic — the CDN request was silently
dropped, leaving `document.readyState = "loading"` indefinitely, blocking screenshots.

**Fix:** Downloaded htmx 1.9.10 to `/opt/lumina-fleet/shared/htmx.min.js` (served by
Apache at `http://192.168.0.120/shared/`). Updated 4 templates:

```html
<!-- before -->
<script src="https://unpkg.com/htmx.org@1.9.10" integrity="sha384-..." crossorigin="anonymous"></script>

<!-- after -->
<script src="http://192.168.0.120/shared/htmx.min.js"></script>
```

Files changed: `base.html`, `skills.html`, `wizard.html`, `wiki.html`

### 2. `spectra_worker.py` — `wait_until` parameter

Added `wait_until` parameter to `navigate()` (default: `"networkidle"`).
HTMX pages require `"domcontentloaded"` or `"commit"` to avoid timeout.

```python
async def navigate(self, session_id, url, headed=False, wait_until="networkidle"):
    try:
        response = await page.goto(url, wait_until=wait_until, timeout=15000)
    except Exception:
        response = await page.goto(url, wait_until="domcontentloaded", timeout=15000)
```

### 3. `spectra_service.py` — `NavigateRequest` model

Added `wait_until` field to `NavigateRequest` and forwarded it to the worker:

```python
class NavigateRequest(BaseModel):
    ...
    wait_until: str = "networkidle"  # use "domcontentloaded" for HTMX pages
```

### 4. Soma JWT auth via `execute_js`

`spectra-internal` browser can't send custom HTTP headers (`X-Soma-Key`) during
navigation. Auth flow used in `spectra_soma_feedback.py`:

1. Navigate to `/login` with `wait_until="domcontentloaded"`
2. Generate HS256 JWT (key: `SOMA_JWT_SECRET`) via Python `hmac` (no `pyjwt` dep)
3. Inject cookie: `execute_js → document.cookie = 'soma_session=JWT; path=/'`
4. Navigate to target page with `wait_until="domcontentloaded"`

---

## Plane

- **BA.19** — Done (comment added with implementation details)
- **BA.20** — Done (comment added with 5 improvements + commit ref)

---

## Files changed

```
fleet/spectra/spectra_soma_feedback.py   (new)
fleet/spectra/spectra_worker.py          (wait_until param + screenshot fix)
fleet/spectra/spectra_service.py         (NavigateRequest.wait_until field)
fleet/soma/templates/base.html           (htmx local)
fleet/soma/templates/skills.html         (htmx local)
fleet/soma/templates/wizard.html         (htmx local)
fleet/soma/templates/wiki.html           (htmx local)
fleet/soma/templates/status.html         (CRITICAL badge, alert banner)
fleet/soma/templates/status_grid.html    (named retry buttons, error truncation)
fleet/soma/templates/security.html       (summary badge)
fleet/soma/templates/logs.html           (log level color coding)
```

---

## Artifacts on CT310

| Path | Contents |
|------|----------|
| `/opt/spectra/spectra_soma_feedback.py` | Feedback engine (deployed) |
| `/data/spectra/feedback/feedback_summary.json` | Latest run aggregate |
| `/data/spectra/feedback/{page}.png` | Before screenshots (11 pages) |
| `/data/spectra/feedback/{page}_after.png` | After screenshots (status, security, logs) |
| `/opt/lumina-fleet/shared/htmx.min.js` | Vendored htmx 1.9.10 |
| `/home/coder/soma-feedback-report.md` | Full report (CT212) |
