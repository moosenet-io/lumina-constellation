"""
spectra_worker.py — Playwright browser worker for Spectra service.
Manages browser contexts, sessions, recordings, and screenshots.
"""

import asyncio
import base64
import io
import json
import os
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

DATA_DIR = Path(os.environ.get("SPECTRA_DATA", "/data/spectra"))
DISPLAY = os.environ.get("DISPLAY", ":99")


class SpectraWorker:
    def __init__(self):
        self._playwright = None
        self._browser = None
        self._contexts: dict[str, object] = {}   # session_id -> BrowserContext
        self._pages: dict[str, object] = {}       # session_id -> Page
        self._recordings: dict[str, list] = {}    # session_id -> rrweb events

    async def start(self):
        from playwright.async_api import async_playwright
        self._playwright = await async_playwright().start()
        self._browser = await self._playwright.chromium.launch(
            headless=True,
            args=[
                "--disable-extensions",
                "--disable-sync",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-features=DnsOverHttps",
                "--no-first-run",
                "--disable-dev-shm-usage",
            ],
        )
        print("[worker] Chromium browser started.")

    async def stop(self):
        if self._browser:
            await self._browser.close()
        if self._playwright:
            await self._playwright.stop()
        print("[worker] Browser stopped.")

    async def chromium_alive(self) -> bool:
        try:
            return self._browser is not None and self._browser.is_connected()
        except Exception:
            return False

    def _get_page(self, session_id: str):
        if session_id not in self._pages:
            raise ValueError(f"Session {session_id} not found")
        return self._pages[session_id]

    async def _get_or_create_context(self, session_id: str, headed: bool = False):
        if session_id not in self._contexts:
            ctx = await self._browser.new_context(
                viewport={"width": 1280, "height": 800},
                ignore_https_errors=True,
                java_script_enabled=True,
            )
            self._contexts[session_id] = ctx
            page = await ctx.new_page()
            self._pages[session_id] = page
            self._recordings[session_id] = []

            # Inject rrweb for recording
            await page.add_init_script("""
                if (typeof window.__rrweb_recording === 'undefined') {
                    window.__rrweb_recording = [];
                    window.__rrweb_flush = function() { return window.__rrweb_recording; };
                }
            """)
        return self._contexts[session_id]

    async def navigate(self, session_id: str, url: str, headed: bool = False) -> dict:
        await self._get_or_create_context(session_id, headed)
        page = self._get_page(session_id)
        response = await page.goto(url, wait_until="networkidle", timeout=30000)
        title = await page.title()
        status = response.status if response else 0
        return {"title": title, "status": status, "url": url}

    async def screenshot(self, session_id: str) -> str:
        page = self._get_page(session_id)
        buf = await page.screenshot(type="png", full_page=False)
        return base64.b64encode(buf).decode()

    async def get_html(self, session_id: str) -> str:
        page = self._get_page(session_id)
        return await page.content()

    async def accessibility_snapshot(self, session_id: str) -> dict:
        page = self._get_page(session_id)
        snapshot = await page.accessibility.snapshot()
        return snapshot or {}

    async def click(self, session_id: str, selector: str):
        page = self._get_page(session_id)
        await page.click(selector, timeout=10000)

    async def type_text(self, session_id: str, selector: str, text: str):
        page = self._get_page(session_id)
        await page.fill(selector, text)

    async def execute_js(self, session_id: str, script: str):
        page = self._get_page(session_id)
        return await page.evaluate(script)

    async def extract_links(self, session_id: str) -> list:
        page = self._get_page(session_id)
        links = await page.evaluate("""
            () => Array.from(document.querySelectorAll('a[href]')).map(a => ({
                href: a.href,
                text: a.innerText.trim().slice(0, 100),
            }))
        """)
        return links or []

    async def fill_form(self, session_id: str, fields: dict):
        page = self._get_page(session_id)
        for selector, value in fields.items():
            await page.fill(selector, value)

    async def wait_for(self, session_id: str, selector: str = None,
                       state: str = "visible", timeout_ms: int = 10000):
        page = self._get_page(session_id)
        if selector:
            await page.wait_for_selector(selector, state=state, timeout=timeout_ms)
        else:
            await page.wait_for_load_state("networkidle", timeout=timeout_ms)

    async def save_pdf(self, session_id: str) -> str:
        page = self._get_page(session_id)
        buf = await page.pdf(format="A4")
        return base64.b64encode(buf).decode()

    async def save_recording(self, session_id: str) -> Path:
        """Save rrweb recording to disk."""
        rec_dir = DATA_DIR / "recordings"
        rec_dir.mkdir(parents=True, exist_ok=True)
        events = self._recordings.get(session_id, [])
        path = rec_dir / f"{session_id}.json"
        path.write_text(json.dumps({
            "session_id": session_id,
            "recorded_at": datetime.now(timezone.utc).isoformat(),
            "events": events,
        }))
        return path

    async def close_session(self, session_id: str):
        if session_id in self._pages:
            try:
                await save_recording(session_id)
            except Exception:
                pass
            try:
                await self._pages[session_id].close()
            except Exception:
                pass
            del self._pages[session_id]
        if session_id in self._contexts:
            try:
                await self._contexts[session_id].close()
            except Exception:
                pass
            del self._contexts[session_id]
        self._recordings.pop(session_id, None)

    async def detect_hitl(self, session_id: str) -> Optional[str]:
        """Detect if human intervention is needed. Returns reason or None."""
        page = self._get_page(session_id)
        try:
            # Check for password fields (login forms)
            pw_field = await page.query_selector("input[type=password]")
            if pw_field:
                return "login_form_detected"
            # Check for CAPTCHA indicators
            content = await page.content()
            captcha_indicators = ["recaptcha", "hcaptcha", "captcha", "cf-challenge"]
            for indicator in captcha_indicators:
                if indicator.lower() in content.lower():
                    return f"captcha_detected:{indicator}"
        except Exception:
            pass
        return None
