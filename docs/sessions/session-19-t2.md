# Session 19 — Terminal 2 Progress
**Date:** 2026-04-16
**Owner:** T2 (all Phase 3 UX items from lumina-doc31-v2-bug-squash)
**Commits:** BS.12, BS.13, BS.14, BS.15

---

## Completed

### BS.13 — Wiki sidebar (warmup)
**Status:** ✅ Done, deployed, committed
**What:** `fleet/soma/templates/wiki.html` was a standalone HTML doc (no Soma sidebar). Refactored to extend `base.html` with `{% extends "base.html" %}`. Two-panel wiki layout preserved inside `{% block content %}` using `margin:-1.5rem` to counteract `.main-content` padding. All JS (renderNav, loadPage, searchWiki) intact.

### BS.12 — Clickable CRITICAL badge with dynamic tooltip popover
**Status:** ✅ Done, deployed, committed
**What:** `fleet/soma/templates/status.html` — CRITICAL/DEGRADED badge becomes a `<button class="critical-badge-btn">`. Clicking opens a popover that scans DOM `.health-dot.dead/.degraded` cards, classifies errors (network/config/offline/degraded), shows colored dots + service names + truncated error text. Click-to-scroll scrolls + 2s outline-highlights the card. Outside-click and Escape close it. Mobile: full-screen overlay. Re-renders on HTMX grid refresh if popover is open.

### BS.14 — Spectra page fix
**Status:** ✅ Done, deployed, committed
**What:**
- `fleet/soma/templates/spectra.html` — replaced broken HTMX `hx-get` status loading with `fetch()` pattern (reads `localStorage.soma_key` for auth). "Loading forever" replaced with proper error state + Retry button. Client-side operator check via `/api/auth/me` (role admin/operator required). Live View iframe src fixed to `/ws/spectra/novnc`. Auto-refresh every 15s.
- `fleet/soma/templates/not_authorized.html` — new page for server-side 403 redirect (T1 TODO: add `@require_operator` guard on `/spectra` route in api/main.py).

### BS.15 — System Status dashboard graphs + density widgets
**Status:** ✅ Done, deployed, committed
**What:** Added after service cards in `status.html`:

| Widget | Size | Data source | Status |
|--------|------|-------------|--------|
| Activity Monitor | wide (3-col) | Prometheus CPU/RAM sparklines for CT305/CT310/CT214/CT300 | Live (Prometheus 192.168.0.222:9090) |
| Request Volume | 2-col | Prometheus LiteLLM rate metric | Graceful empty if metric absent |
| Engram Growth | 1-col | `/api/status` → services.engram.fact_count | Live |
| Spend Tracker | 2-col | `/api/cost` → today + per_agent | Live (Myelin data) |
| Recent Activity | wide (3-col) | `/api/spectra/audit` (proxy); Nexus feed pending | Partial — Nexus T1 needed |
| Household Status | 1-col | `/api/status` → services.agents.agents map | Live |
| Invite Card | 1-col | `/api/invites` POST (BS.16 backend pending) | Stub UI only |

**Chart.js 4.4.4** vendored to `/opt/lumina-fleet/shared/chart.umd.min.js` (also `fleet/shared/chart.umd.min.js` in repo).

Responsive: 3-col → 2-col at 900px → 1-col at 600px.

---

## Pending items (T1 territory)

These were noted during T2 build and require backend changes (`fleet/soma/api/*.py`):

| Item | What's needed | Priority |
|------|--------------|---------|
| Spectra server-side guard | Add `@require_operator` on `GET /spectra` route; return 403 → renders `not_authorized.html` | BS.14 T1 TODO |
| Nexus activity feed | `/api/nexus/activity` or `/api/activity` endpoint for Recent Activity widget | BS.15 |
| Invite API | `POST /api/invites`, `GET /api/invites` for invite modal | BS.16 |
| Prometheus proxy | Optional: proxy Prometheus queries through Soma API so browser doesn't need direct cluster access | Hardening |

---

## Commits
- `BS.13: wiki.html — refactor to extend base.html, adds Soma sidebar navigation` (earlier session, see session-18-t2.md)
- `BS.12: Clickable CRITICAL/DEGRADED badge with dynamic categorized tooltip popover`
- `BS.14: Spectra page — Live View fix + operator-only access check`
- `BS.15: System Status dashboard graphs + density widgets`
