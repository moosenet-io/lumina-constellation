"""
spectra_soma_feedback.py — Soma visual feedback loop engine (BA.19)

Navigates each Soma admin page via spectra-internal, takes accessibility snapshots
and screenshots, sends them to the local Qwen LLM for UX analysis, stores results
in Engram and JSON files in /data/spectra/feedback/.

Usage (on the fleet host):
    python3 spectra_soma_feedback.py [--page STATUS]

Environment:
    SPECTRA_INTERNAL_URL  http://YOUR_SPECTRA_HOST:8085
    LITELLM_URL           http://YOUR_LITELLM_HOST:4000
    LITELLM_MASTER_KEY    (from env)
    SOMA_JWT_SECRET       (from env)
    ENGRAM_DB_PATH        /opt/lumina-fleet/engram/engram.db
"""

import argparse
import base64
import hashlib
import hmac
import json
import os
import sys
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path


# ── Config ────────────────────────────────────────────────────────────────────
SPECTRA_URL   = os.environ.get("SPECTRA_INTERNAL_URL", "")
LITELLM_URL   = os.environ.get("LITELLM_URL", "")
LITELLM_KEY   = os.environ.get("LITELLM_MASTER_KEY", "")
SOMA_BASE     = os.environ.get("SOMA_URL", "")
SOMA_JWT_KEY  = os.environ.get("SOMA_JWT_SECRET",
                os.environ.get("SOMA_SECRET_KEY", ""))
CONSUMER_KEY  = "MY.1"
OUTPUT_DIR    = Path(os.environ.get("SPECTRA_DATA", "/data/spectra")) / "feedback"
FLEET_DIR     = Path(os.environ.get("FLEET_DIR", "/opt/lumina-fleet"))

# Pages to analyse (name, path, description)
SOMA_PAGES = [
    ("status",    "/status",    "System status dashboard — service health grid"),
    ("config",    "/config",    "Configuration editor — service toggles and settings"),
    ("security",  "/security",  "Security audit and access control panel"),
    ("skills",    "/skills",    "Skills library — prompt macros and procedures"),
    ("plugins",   "/plugins",   "Plugin management — agent module toggles"),
    ("sessions",  "/sessions",  "Active session viewer"),
    ("logs",      "/logs",      "Log stream viewer"),
    ("vector",    "/vector",    "Vector agent — dev loop management"),
    ("synapse",   "/synapse",   "Synapse — Matrix bridge and notifications"),
    ("spectra",   "/spectra",   "Spectra — live browser view and recordings"),
    ("council",   "/council",   "Obsidian Circle council — reasoning sessions"),
]

# LLM analysis prompt
UX_ANALYSIS_PROMPT = """You are a UX analyst reviewing admin panel screenshots and accessibility data for an AI automation system called Lumina. The operator is a non-technical user who directs AI agents through a web panel.

Page: {page_name}
Description: {page_desc}

Accessibility snapshot (role tree):
{accessibility}

Analyze this admin panel page for UX issues and improvements. Focus on:
1. Information hierarchy — is the most important data visible first?
2. Status clarity — can the operator tell at a glance if things are working?
3. Actionability — are CTAs and controls clearly labeled and discoverable?
4. Error visibility — are errors/warnings prominent enough?
5. Navigation — is it easy to move between related tasks?

Return a JSON array of findings. Each finding:
{{
  "severity": "critical|high|medium|low",
  "category": "hierarchy|clarity|actionability|errors|navigation|other",
  "finding": "One sentence description of the issue",
  "suggestion": "One sentence concrete fix",
  "element": "CSS selector or element name if applicable, else null"
}}

Return ONLY the JSON array, no commentary."""


# ── Helpers ───────────────────────────────────────────────────────────────────

def _b64url(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def _make_jwt(secret: str) -> str:
    """Create a HS256 JWT for Soma admin access."""
    header  = _b64url(b'{"alg":"HS256","typ":"JWT"}')
    payload = _b64url(json.dumps({
        "sub": "admin", "role": "admin",
        "exp": int(time.time()) + 86400
    }).encode())
    msg = f"{header}.{payload}".encode()
    sig = _b64url(hmac.new(secret.encode(), msg, hashlib.sha256).digest())
    return f"{header}.{payload}.{sig}"


def _post(url: str, data: dict, timeout: int = 20) -> dict:
    """HTTP POST with JSON body, returns parsed JSON or raises."""
    body = json.dumps(data).encode()
    req  = urllib.request.Request(url, data=body,
                                   headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def _get_json(url: str, timeout: int = 10) -> dict:
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return json.loads(r.read())


def navigate(session_id: str, url: str, wait: str = "domcontentloaded") -> dict:
    return _post(f"{SPECTRA_URL}/navigate", {
        "url": url, "session_id": session_id,
        "consumer_key": CONSUMER_KEY, "wait_until": wait,
    }, timeout=25)


def execute_js(session_id: str, script: str) -> dict:
    return _post(f"{SPECTRA_URL}/execute_js", {
        "session_id": session_id,
        "consumer_key": CONSUMER_KEY,
        "script": script,
    }, timeout=10)


def screenshot(session_id: str) -> str:
    """Returns base64 PNG or empty string on failure."""
    url = f"{SPECTRA_URL}/screenshot?session_id={session_id}&consumer_key={CONSUMER_KEY}"
    req = urllib.request.Request(url, data=b"", method="POST")
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            d = json.loads(r.read())
            return d.get("png_b64", "")
    except Exception as e:
        print(f"  [screenshot] failed: {e}")
        return ""


def accessibility_snapshot(session_id: str) -> dict:
    url = f"{SPECTRA_URL}/accessibility_snapshot?session_id={session_id}&consumer_key={CONSUMER_KEY}"
    req = urllib.request.Request(url, data=b"", method="POST")
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            d = json.loads(r.read())
            return d.get("snapshot", {})
    except Exception as e:
        print(f"  [a11y] failed: {e}")
        return {}


def close_session(session_id: str):
    url = f"{SPECTRA_URL}/session/close?session_id={session_id}&consumer_key={CONSUMER_KEY}"
    try:
        req = urllib.request.Request(url, data=b"", method="POST")
        urllib.request.urlopen(req, timeout=5)
    except Exception:
        pass


def _flatten_a11y(node: dict, depth: int = 0) -> str:
    """Flatten accessibility tree to text for LLM prompt."""
    if not node:
        return ""
    lines = []
    indent = "  " * depth
    role  = node.get("role", "")
    name  = node.get("name", "")
    level = node.get("level", "")
    if role and name:
        level_str = f" (h{level})" if level else ""
        lines.append(f"{indent}{role}{level_str}: {name!r}")
    for child in node.get("children", []):
        lines.append(_flatten_a11y(child, depth + 1))
    return "\n".join(l for l in lines if l)


def call_llm(prompt: str) -> str:
    """Call LiteLLM 'Lumina Fast' (Qwen) for UX analysis."""
    url  = f"{LITELLM_URL}/chat/completions"
    data = {
        "model": "Lumina Fast",
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.3,
        "max_tokens": 1500,
    }
    headers = {"Content-Type": "application/json"}
    if LITELLM_KEY:
        headers["Authorization"] = f"Bearer {LITELLM_KEY}"

    body = json.dumps(data).encode()
    req  = urllib.request.Request(url, data=body, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            resp = json.loads(r.read())
            return resp["choices"][0]["message"]["content"].strip()
    except Exception as e:
        print(f"  [llm] failed: {e}")
        return "[]"


def parse_llm_findings(raw: str) -> list:
    """Extract JSON array from LLM response."""
    # Try to find JSON array in response
    raw = raw.strip()
    start = raw.find("[")
    end   = raw.rfind("]")
    if start == -1 or end == -1:
        return []
    try:
        findings = json.loads(raw[start:end + 1])
        if isinstance(findings, list):
            return findings
    except json.JSONDecodeError:
        pass
    return []


def store_in_engram(page_name: str, findings: list, session_id: str):
    """Store findings in Engram knowledge base."""
    try:
        sys.path.insert(0, str(FLEET_DIR / "engram"))
        import engram
        for i, f in enumerate(findings):
            key = f"soma-feedback/{page_name}/{f.get('category','other')}/{i}"
            content = (
                f"[Soma UX Feedback: {page_name}] "
                f"Severity: {f.get('severity','?')} | "
                f"Category: {f.get('category','?')} | "
                f"{f.get('finding','')} — Fix: {f.get('suggestion','')}"
            )
            engram.store(key, content, layer='kb',
                         tags=['soma-feedback', page_name, f.get('severity',''), 'ux'],
                         source_agent='spectra-feedback')
    except Exception as e:
        print(f"  [engram] store failed: {e}")


# ── Main ──────────────────────────────────────────────────────────────────────

def analyse_page(page_name: str, page_path: str, page_desc: str,
                 session_id: str, jwt: str, output_dir: Path) -> dict:
    """Analyse one Soma page. Returns dict with findings."""
    url = f"{SOMA_BASE}{page_path}"
    print(f"\n  → {page_name} ({url})")

    # Set auth cookie
    try:
        execute_js(session_id, f"(()=>{{document.cookie='soma_session={jwt};path=/';return 'ok'}})()")
    except Exception as e:
        print(f"    [cookie] {e}")

    # Navigate
    try:
        nav = navigate(session_id, url, wait="domcontentloaded")
        print(f"    navigated: {nav.get('title', '?')}")
    except Exception as e:
        print(f"    [navigate] timeout/error (page likely loaded): {e}")
        # Re-set cookie in case redirect cleared it
        try:
            execute_js(session_id, f"(()=>{{document.cookie='soma_session={jwt};path=/';return 'ok'}})()")
        except Exception:
            pass

    # Verify we're on the right page
    try:
        loc = execute_js(session_id, "(() => ({url: window.location.href, title: document.title}))()")
        print(f"    location: {loc.get('result', {})}")
    except Exception:
        pass

    # Take screenshot
    png_b64 = screenshot(session_id)
    if png_b64:
        png_path = output_dir / f"{page_name}.png"
        png_path.write_bytes(base64.b64decode(png_b64))
        print(f"    screenshot: {png_path} ({len(png_b64) * 3 // 4} bytes)")
    else:
        print(f"    screenshot: failed")

    # Get accessibility snapshot
    a11y = accessibility_snapshot(session_id)
    a11y_text = _flatten_a11y(a11y)

    # Build LLM prompt
    prompt = UX_ANALYSIS_PROMPT.format(
        page_name=page_name,
        page_desc=page_desc,
        accessibility=a11y_text[:3000] if a11y_text else "(no accessibility data)",
    )

    # Call Qwen via LiteLLM
    print(f"    calling LLM...")
    raw_response = call_llm(prompt)
    findings = parse_llm_findings(raw_response)
    print(f"    findings: {len(findings)}")

    # Build result
    result = {
        "page": page_name,
        "url": url,
        "analysed_at": datetime.now(timezone.utc).isoformat(),
        "session_id": session_id,
        "has_screenshot": bool(png_b64),
        "accessibility_nodes": len(a11y.get("children", [])),
        "findings": findings,
        "raw_llm_response": raw_response,
    }

    # Save JSON
    json_path = output_dir / f"{page_name}.json"
    json_path.write_text(json.dumps(result, indent=2))

    # Store in Engram
    if findings:
        store_in_engram(page_name, findings, session_id)

    return result


def run(pages_filter: list = None) -> list:
    """Run full feedback loop. Returns list of all page results."""
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    pages = SOMA_PAGES if not pages_filter else [
        p for p in SOMA_PAGES if p[0].lower() in [f.lower() for f in pages_filter]
    ]

    if not pages:
        print(f"No matching pages for filter: {pages_filter}")
        return []

    print(f"[spectra-soma-feedback] Analysing {len(pages)} Soma pages")
    print(f"  Spectra: {SPECTRA_URL}")
    print(f"  Soma: {SOMA_BASE}")
    print(f"  Output: {OUTPUT_DIR}")

    # Create one browser session for the whole run
    print("\n[1/4] Creating browser session...")
    resp = _post(f"{SPECTRA_URL}/navigate", {
        "url": f"{SOMA_BASE}/login",
        "consumer_key": CONSUMER_KEY,
        "wait_until": "domcontentloaded",
    }, timeout=20)
    session_id = resp.get("session_id")
    if not session_id:
        raise RuntimeError(f"Failed to create session: {resp}")
    print(f"  Session: {session_id}")

    # Generate JWT
    jwt = _make_jwt(SOMA_JWT_KEY)
    print(f"  JWT created (admin, 24h)")

    # Set auth cookie on login page
    execute_js(session_id, f"(()=>{{document.cookie='soma_session={jwt};path=/';return 'ok'}})()")
    print(f"  Auth cookie set")

    # Analyse each page
    print(f"\n[2/4] Analysing pages...")
    all_results = []
    for page_name, page_path, page_desc in pages:
        result = analyse_page(page_name, page_path, page_desc,
                              session_id, jwt, OUTPUT_DIR)
        all_results.append(result)

    # Close session
    print(f"\n[3/4] Closing session...")
    close_session(session_id)

    # Build summary
    print(f"\n[4/4] Building summary...")
    total_findings = sum(len(r["findings"]) for r in all_results)
    critical = sum(1 for r in all_results for f in r["findings"]
                   if f.get("severity") == "critical")
    high     = sum(1 for r in all_results for f in r["findings"]
                   if f.get("severity") == "high")

    summary = {
        "run_at": datetime.now(timezone.utc).isoformat(),
        "pages_analysed": len(all_results),
        "total_findings": total_findings,
        "severity_summary": {"critical": critical, "high": high,
                              "medium": total_findings - critical - high},
        "pages": [
            {
                "page": r["page"],
                "findings": len(r["findings"]),
                "top_severity": max(
                    (f.get("severity", "low") for f in r["findings"]),
                    key=lambda s: {"critical": 4, "high": 3, "medium": 2, "low": 1}.get(s, 0),
                    default="none",
                ),
            }
            for r in all_results
        ],
        "all_findings": [
            {**f, "page": r["page"]}
            for r in all_results
            for f in r["findings"]
        ],
    }

    summary_path = OUTPUT_DIR / "feedback_summary.json"
    summary_path.write_text(json.dumps(summary, indent=2))
    print(f"  Summary: {summary_path}")
    print(f"  Total findings: {total_findings} ({critical} critical, {high} high)")

    return all_results


# ── CLI ───────────────────────────────────────────────────────────────────────

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Soma visual feedback loop engine")
    parser.add_argument("--page", nargs="+", metavar="NAME",
                        help="Only analyse specific pages (e.g. --page status config)")
    parser.add_argument("--output-dir", default=None,
                        help="Override output directory")
    parser.add_argument("--list-pages", action="store_true",
                        help="List available pages and exit")
    args = parser.parse_args()

    if args.list_pages:
        print("Available pages:")
        for name, path, desc in SOMA_PAGES:
            print(f"  {name:<12} {path:<20} {desc}")
        sys.exit(0)

    if args.output_dir:
        OUTPUT_DIR = Path(args.output_dir)

    results = run(pages_filter=args.page)
    print(f"\nDone. {len(results)} pages analysed.")
