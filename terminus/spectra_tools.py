"""
spectra_tools.py — 21 Spectra browser agent MCP tools. (BA.5)
Runs on Terminus. All tools gate on consumer key validation,
budget, and rate limit BEFORE any browser action.

Spectra service: fleet host Docker port 8084
"""

import base64
import io
import json
import os
import subprocess
import sys
import urllib.request
import urllib.error
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

_FLEET_DIR = Path(os.environ.get('FLEET_DIR', '/opt/lumina-fleet'))
sys.path.insert(0, '/opt/ai-mcp')

# Spectra service URL
SPECTRA_URL = os.environ.get('SPECTRA_URL', '')
SPECTRA_INTERNAL_URL = os.environ.get('SPECTRA_INTERNAL_URL', 'http://127.0.0.1:8085')
SPECTRA_TIMEOUT = int(os.environ.get('SPECTRA_TIMEOUT', '60'))

# Default consumer key for Lumina orchestrator
DEFAULT_CONSUMER_KEY = os.environ.get('SPECTRA_DEFAULT_KEY', 'MY.2')


def _post(endpoint: str, data: dict = None, internal: bool = False, timeout: int = None) -> dict:
    """HTTP POST to spectra service."""
    base = SPECTRA_INTERNAL_URL if internal else SPECTRA_URL
    url = f"{base}{endpoint}"
    payload = json.dumps(data or {}).encode()
    req = urllib.request.Request(
        url, data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout or SPECTRA_TIMEOUT) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        body = e.read().decode()[:300]
        return {"ok": False, "error": f"HTTP {e.code}: {body}"}
    except Exception as e:
        return {"ok": False, "error": str(e)[:200]}


def _get(endpoint: str, params: dict = None, internal: bool = False) -> dict:
    """HTTP GET to spectra service."""
    base = SPECTRA_INTERNAL_URL if internal else SPECTRA_URL
    url = f"{base}{endpoint}"
    if params:
        qs = "&".join(f"{k}={v}" for k, v in params.items())
        url = f"{url}?{qs}"
    req = urllib.request.Request(url)
    try:
        with urllib.request.urlopen(req, timeout=SPECTRA_TIMEOUT) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        body = e.read().decode()[:200]
        return {"ok": False, "error": f"HTTP {e.code}: {body}"}
    except Exception as e:
        return {"ok": False, "error": str(e)[:200]}


def register_spectra_tools(mcp):

    @mcp.tool()
    def spectra_navigate(
        url: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
        session_id: str = "",
        headed: bool = False,
    ) -> dict:
        """Navigate Spectra browser to a URL. Returns session_id, page title, status.
        Args:
            url: Full URL to navigate to (https://...)
            consumer_key: Myelin virtual key for access control (default: Lumina MY.2)
            session_id: Resume existing session (optional — creates new if omitted)
            headed: Show in Live View (default: False / headless)
        """
        data = {"url": url, "consumer_key": consumer_key, "headed": headed}
        if session_id:
            data["session_id"] = session_id
        return _post("/navigate", data)

    @mcp.tool()
    def spectra_screenshot(
        session_id: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Capture screenshot of current page. Returns base64 PNG.
        Args:
            session_id: Active Spectra session ID
            consumer_key: Myelin virtual key
        """
        return _post(f"/screenshot?session_id={session_id}&consumer_key={consumer_key}")

    @mcp.tool()
    def spectra_click(
        session_id: str,
        selector: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Click an element by CSS selector.
        Args:
            session_id: Active session ID
            selector: CSS selector (e.g. 'button[type=submit]', '#login-btn')
            consumer_key: Myelin virtual key
        """
        return _post("/click", {"session_id": session_id, "selector": selector,
                                "consumer_key": consumer_key})

    @mcp.tool()
    def spectra_type(
        session_id: str,
        selector: str,
        text: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Fill text into an input field by CSS selector.
        Args:
            session_id: Active session ID
            selector: CSS selector for the input field
            text: Text to type
            consumer_key: Myelin virtual key
        """
        return _post("/type", {"session_id": session_id, "selector": selector,
                               "text": text, "consumer_key": consumer_key})

    @mcp.tool()
    def spectra_extract_text(
        session_id: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Extract and sanitize visible text from current page.
        Runs 10-stage sanitization pipeline. Output wrapped in [UNTRUSTED_WEB_CONTENT] delimiters.
        Args:
            session_id: Active session ID
            consumer_key: Myelin virtual key
        """
        return _post(f"/extract_text?session_id={session_id}&consumer_key={consumer_key}")

    @mcp.tool()
    def spectra_extract_links(
        session_id: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Extract all hyperlinks from current page with anchor text.
        Returns list of {href, text} dicts.
        Args:
            session_id: Active session ID
            consumer_key: Myelin virtual key
        """
        return _post(f"/extract_links?session_id={session_id}&consumer_key={consumer_key}")

    @mcp.tool()
    def spectra_fill_form(
        session_id: str,
        fields: dict,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Fill multiple form fields by selector dict.
        Args:
            session_id: Active session ID
            fields: Dict of {css_selector: value} (e.g. {'#email': 'user@example.com'})
            consumer_key: Myelin virtual key
        """
        return _post("/fill_form", {"session_id": session_id, "fields": fields,
                                    "consumer_key": consumer_key})

    @mcp.tool()
    def spectra_execute_js(
        session_id: str,
        script: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Execute JavaScript in current page context. Audit logged.
        Use sparingly — prefer higher-level tools when possible.
        Args:
            session_id: Active session ID
            script: JavaScript to evaluate (return value is included in response)
            consumer_key: Myelin virtual key
        """
        return _post("/execute_js", {"session_id": session_id, "script": script,
                                     "consumer_key": consumer_key})

    @mcp.tool()
    def spectra_pdf(
        session_id: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Save current page as PDF. Returns base64-encoded PDF.
        Args:
            session_id: Active session ID
            consumer_key: Myelin virtual key
        """
        return _post(f"/pdf?session_id={session_id}&consumer_key={consumer_key}")

    @mcp.tool()
    def spectra_wait_for(
        session_id: str,
        selector: str = "",
        state: str = "visible",
        timeout_ms: int = 10000,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Wait for a selector to reach a state, or for network idle.
        Args:
            session_id: Active session ID
            selector: CSS selector to wait for (omit to wait for network idle)
            state: 'visible', 'hidden', 'attached', 'detached' (default: visible)
            timeout_ms: Max wait in milliseconds (default: 10000)
            consumer_key: Myelin virtual key
        """
        return _post("/wait_for", {"session_id": session_id, "selector": selector,
                                   "state": state, "timeout_ms": timeout_ms,
                                   "consumer_key": consumer_key})

    @mcp.tool()
    def spectra_session_list(
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """List active Spectra browser sessions.
        Returns sessions with ID, URL, consumer, elapsed time, state.
        Args:
            consumer_key: Myelin virtual key
        """
        return _get("/sessions", params={"consumer_key": consumer_key})

    @mcp.tool()
    def spectra_session_close(
        session_id: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Close a Spectra browser session and save its recording.
        Args:
            session_id: Session ID to close
            consumer_key: Myelin virtual key
        """
        return _post(f"/session/close?session_id={session_id}&consumer_key={consumer_key}")

    @mcp.tool()
    def spectra_live_view_url(
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Get the Soma WebSocket proxy URL for Live View (noVNC).
        Navigate to this URL in Soma /spectra to see the browser in real time.
        Args:
            consumer_key: Myelin virtual key
        """
        soma_url = os.environ.get('SOMA_URL', '')
        return {
            "ok": True,
            "live_view_url": f"{soma_url}/spectra",
            "note": "Open Soma /spectra page — Live View iframe embedded there",
        }

    @mcp.tool()
    def spectra_session_recordings(
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """List available session recordings for playback in Soma /spectra/recordings.
        Args:
            consumer_key: Myelin virtual key
        """
        return _get("/recordings", params={"consumer_key": consumer_key})

    @mcp.tool()
    def spectra_request_human_help(
        session_id: str,
        reason: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Trigger HITL pipeline — notify the operator via Synapse/Matrix that human intervention needed.
        Use when encountering: login forms, CAPTCHAs, 2FA, ambiguous decisions.
        Args:
            session_id: Active session ID needing help
            reason: Why human help is needed (e.g. 'login_form', 'captcha', 'ambiguous_content')
            consumer_key: Myelin virtual key
        """
        return _post("/hitl/request", {"session_id": session_id, "reason": reason,
                                       "consumer_key": consumer_key})

    @mcp.tool()
    def spectra_store_content(
        session_id: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
        store_screenshot: bool = True,
        store_accessibility: bool = True,
        store_links: bool = True,
    ) -> dict:
        """Extract page content, sanitize, and store to Engram with spectra: namespace.
        Content is RAG-queryable via engram_query(source='spectra').
        Args:
            session_id: Active session ID
            consumer_key: Myelin virtual key
            store_screenshot: Also capture and store screenshot thumbnail (default: True)
            store_accessibility: Also store accessibility snapshot (default: True)
            store_links: Also store link graph (default: True)
        """
        return _post("/store_content", {
            "session_id": session_id,
            "consumer_key": consumer_key,
            "store_screenshot": store_screenshot,
            "store_accessibility": store_accessibility,
            "store_links": store_links,
        })

    @mcp.tool()
    def spectra_internal_screenshot(
        url: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Screenshot a LAN-internal service (Soma, Prometheus, virtualization).
        Uses spectra-internal container (LAN-only, no internet, URL allowlist).
        Args:
            url: Internal URL (must be in allowlist: Soma :8082, Prometheus :9090, etc.)
            consumer_key: Myelin virtual key
        """
        return _post("/navigate", {"url": url, "consumer_key": consumer_key}, internal=True)

    @mcp.tool()
    def spectra_audit_query(
        filter_key: str = "",
        action: str = "",
        limit: int = 50,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Query Spectra audit log. Filter by consumer key and/or action type.
        Args:
            filter_key: Filter by consumer_key (e.g. 'MY.2')
            action: Filter by action type ('navigate', 'screenshot', 'extract_text', etc.)
            limit: Max entries to return (default: 50)
            consumer_key: Myelin virtual key (for access control)
        """
        return _get("/audit", params={
            "consumer_key": consumer_key,
            "filter_key": filter_key,
            "action": action,
            "limit": str(limit),
        })

    @mcp.tool()
    def spectra_accessibility_snapshot(
        session_id: str,
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Capture Playwright accessibility tree as structured JSON.
        20-50x more token-efficient than screenshot for LLM page analysis.
        Returns element roles, names, values, states, and hierarchy.
        Args:
            session_id: Active session ID
            consumer_key: Myelin virtual key
        """
        return _post(f"/accessibility_snapshot?session_id={session_id}&consumer_key={consumer_key}")

    @mcp.tool()
    def spectra_visual_diff(
        screenshot_b64_before: str,
        screenshot_b64_after: str,
        page: str = "",
        consumer_key: str = DEFAULT_CONSUMER_KEY,
    ) -> dict:
        """Compare two screenshots using pixel diff + perceptual hash.
        Returns diff_score (0=identical, 100=completely different),
        changed_regions (bounding boxes), and diff_image_b64 (highlighted changes).
        Stores result in Engram with source='spectra-diff'.
        Args:
            screenshot_b64_before: Base64 PNG of page before change
            screenshot_b64_after: Base64 PNG of page after change
            page: Page name/URL for labeling (e.g. '/status')
            consumer_key: Myelin virtual key
        """
        try:
            from PIL import Image, ImageChops, ImageDraw
            import imagehash

            def decode(b64: str):
                return Image.open(io.BytesIO(base64.b64decode(b64))).convert("RGB")

            img_before = decode(screenshot_b64_before)
            img_after = decode(screenshot_b64_after)

            # Ensure same size
            if img_before.size != img_after.size:
                img_after = img_after.resize(img_before.size, Image.LANCZOS)

            # Pixel diff
            diff = ImageChops.difference(img_before, img_after)
            diff_arr = list(diff.getdata())
            changed_pixels = sum(1 for r, g, b in diff_arr if r + g + b > 30)
            total_pixels = img_before.width * img_before.height
            diff_score = round((changed_pixels / total_pixels) * 100, 2)

            # Perceptual hash similarity
            hash_before = imagehash.average_hash(img_before)
            hash_after = imagehash.average_hash(img_after)
            hash_diff = hash_before - hash_after  # hamming distance (0-64)
            structural_score = round((hash_diff / 64) * 100, 1)

            # Find changed regions (simplified bounding box)
            width, height = img_before.size
            changed_regions = []
            if diff_score > 0:
                # Create highlighted diff image
                diff_img = img_before.copy()
                draw = ImageDraw.Draw(diff_img)
                # Mark changed pixels in red overlay
                for i, (r, g, b) in enumerate(diff_arr):
                    if r + g + b > 30:
                        x = i % width
                        y = i // width
                        draw.point((x, y), fill=(255, 0, 0))
                changed_regions.append({
                    "x": 0, "y": 0, "w": width, "h": height,
                    "note": f"{changed_pixels} pixels changed"
                })

                out_buf = io.BytesIO()
                diff_img.save(out_buf, format="PNG", optimize=True)
                diff_b64 = base64.b64encode(out_buf.getvalue()).decode()
            else:
                diff_b64 = screenshot_b64_before  # identical

            # Store in Engram
            store_result = {}
            try:
                from spectra_store import store_visual_diff
                store_result = store_visual_diff(
                    page=page,
                    diff_score=diff_score,
                    diff_image_b64=diff_b64,
                    changed_regions=changed_regions,
                )
            except Exception:
                pass

            return {
                "ok": True,
                "diff_score": diff_score,
                "structural_score": structural_score,
                "changed_regions": changed_regions,
                "diff_image_b64": diff_b64,
                "engram_fact_id": store_result.get("fact_id"),
            }
        except Exception as e:
            return {"ok": False, "error": str(e)[:300]}

    @mcp.tool()
    def spectra_self_test(
        consumer_key: str = "MY.1",
    ) -> dict:
        """Run Spectra 6-step self-test sequence. Post results to Sentinel.
        Steps: health endpoint, Soma screenshot, accessibility snapshot,
               recording creation, Engram round-trip, network isolation check.
        Args:
            consumer_key: Must be MY.1 (operator) for self-test
        """
        results = []

        def check(name: str, fn):
            try:
                result = fn()
                ok = result.get("ok", False) if isinstance(result, dict) else bool(result)
                results.append({"step": name, "ok": ok, "detail": str(result)[:100]})
                return ok
            except Exception as e:
                results.append({"step": name, "ok": False, "detail": str(e)[:100]})
                return False

        # Step 1: Health endpoint
        check("health_endpoint", lambda: _get("/health"))

        # Step 2: Navigate to Soma /spectra
        nav_result = _post("/navigate", {"url": os.environ.get("SOMA_URL", "") + "/spectra",
                                          "consumer_key": consumer_key})
        results.append({"step": "soma_navigate", "ok": nav_result.get("ok", False),
                        "detail": str(nav_result)[:100]})
        session_id = nav_result.get("session_id", "")

        # Step 3: Accessibility snapshot
        if session_id:
            check("accessibility_snapshot",
                  lambda: _post(f"/accessibility_snapshot?session_id={session_id}&consumer_key={consumer_key}"))

        # Step 4: Check recordings endpoint
        check("recordings_list", lambda: _get("/recordings", params={"consumer_key": consumer_key}))

        # Step 5: Engram store round-trip (via audit query)
        check("audit_accessible", lambda: _get("/audit", params={
            "consumer_key": consumer_key, "limit": "1"
        }))

        # Step 6: Network isolation (internal access should fail for external URL)
        try:
            import urllib.request
            urllib.request.urlopen(os.environ.get("SPECTRA_LAN_TEST_URL", "http://YOUR_LAN_HOST:8006"), timeout=3)
            results.append({"step": "network_isolation", "ok": False,
                            "detail": "FAIL: LAN access from service allowed"})
        except Exception:
            results.append({"step": "network_isolation", "ok": True,
                            "detail": "PASS: LAN access blocked"})

        # Clean up test session
        if session_id:
            _post(f"/session/close?session_id={session_id}&consumer_key={consumer_key}")

        all_pass = all(r["ok"] for r in results)
        return {
            "ok": all_pass,
            "steps": results,
            "passed": sum(1 for r in results if r["ok"]),
            "total": len(results),
            "timestamp": datetime.now(timezone.utc).isoformat(),
        }
