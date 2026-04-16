"""
spectra_store.py — Spectra content storage with Engram integration. (BA.6)
Runs on Terminus. Persists extracted web content to Engram sqlite-vec
with spectra: namespace. Handles dedup, zettelkasten linking, thumbnails.
"""

import base64
import hashlib
import io
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

# Engram Python path on fleet host
_FLEET_DIR = Path(os.environ.get('FLEET_DIR', '/opt/lumina-fleet'))
sys.path.insert(0, str(_FLEET_DIR))
sys.path.insert(0, str(_FLEET_DIR / 'shared'))

SPECTRA_NAMESPACE = "spectra"
SPECTRA_FEEDBACK_NS = "spectra-feedback"
SPECTRA_DIFF_NS = "spectra-diff"
SPECTRA_SNAPSHOT_NS = "spectra-snapshot"
REMOTE_SSH_HOST = os.environ.get("REMOTE_SSH_HOST", "")
FLEET_REMOTE_TARGET = os.environ.get("FLEET_REMOTE_TARGET", "")
REMOTE_EXEC_TEMPLATE = os.environ.get("REMOTE_EXEC_TEMPLATE", "")


def _remote_exec(command: str) -> str:
    if not REMOTE_EXEC_TEMPLATE:
        return ""
    return REMOTE_EXEC_TEMPLATE.format(target=FLEET_REMOTE_TARGET, command=command)


def _engram_store(text: str, metadata: dict, namespace: str = SPECTRA_NAMESPACE) -> Optional[str]:
    """
    Store content to Engram via engram.py CLI on fleet host.
    Returns fact_id or None on failure.
    """
    if not (REMOTE_SSH_HOST and FLEET_REMOTE_TARGET):
        return None
    try:
        payload = json.dumps({
            "text": text,
            "source": namespace,
            "metadata": metadata,
        })
        # Call engram_store via SSH to fleet host
        remote_cmd = _remote_exec("python3 /opt/lumina-fleet/engram/engram.py store")
        if not remote_cmd:
            return None
        result = subprocess.run(
            ["ssh", "-o", "ConnectTimeout=5", REMOTE_SSH_HOST, remote_cmd],
            input=payload.encode(),
            capture_output=True,
            timeout=15,
        )
        if result.returncode == 0:
            out = json.loads(result.stdout.decode().strip())
            return out.get("fact_id")
    except Exception as e:
        print(f"[spectra_store] Engram store error: {e}")
    return None


def _engram_query(query: str, source: str = SPECTRA_NAMESPACE, limit: int = 10) -> list:
    """Query Engram for spectra-sourced content."""
    if not (REMOTE_SSH_HOST and FLEET_REMOTE_TARGET):
        return []
    try:
        payload = json.dumps({"query": query, "source": source, "limit": limit})
        remote_cmd = _remote_exec("python3 /opt/lumina-fleet/engram/engram.py query")
        if not remote_cmd:
            return []
        result = subprocess.run(
            ["ssh", "-o", "ConnectTimeout=5", REMOTE_SSH_HOST, remote_cmd],
            input=payload.encode(),
            capture_output=True,
            timeout=15,
        )
        if result.returncode == 0:
            return json.loads(result.stdout.decode().strip()).get("results", [])
    except Exception as e:
        print(f"[spectra_store] Engram query error: {e}")
    return []


def _url_date_key(url: str) -> str:
    """Dedup key: hash of URL + current date."""
    date = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    return hashlib.md5(f"{url}:{date}".encode()).hexdigest()[:12]


def _make_thumbnail(png_b64: str, max_px: int = 200) -> str:
    """Resize screenshot to thumbnail. Returns base64 PNG."""
    try:
        from PIL import Image
        buf = io.BytesIO(base64.b64decode(png_b64))
        img = Image.open(buf)
        img.thumbnail((max_px, max_px), Image.LANCZOS)
        out = io.BytesIO()
        img.save(out, format="PNG", optimize=True)
        return base64.b64encode(out.getvalue()).decode()
    except Exception as e:
        print(f"[spectra_store] Thumbnail error: {e}")
        return ""


def store_page_content(
    url: str,
    title: str,
    text: str,
    consumer_key: str = "",
    http_status: int = 200,
    screenshot_b64: str = "",
    accessibility_snapshot: dict = None,
    links: list = None,
) -> dict:
    """
    Store extracted page content to Engram with dedup and Zettelkasten linking.

    Returns: {ok, fact_id, dedup_key, thumbnail_stored}
    """
    dedup_key = _url_date_key(url)
    timestamp = datetime.now(timezone.utc).isoformat()

    # Generate thumbnail if screenshot provided
    thumbnail_b64 = ""
    if screenshot_b64:
        thumbnail_b64 = _make_thumbnail(screenshot_b64)

    metadata = {
        "url": url,
        "title": title,
        "timestamp": timestamp,
        "http_status": http_status,
        "consumer_key": consumer_key,
        "dedup_key": dedup_key,
        "has_thumbnail": bool(thumbnail_b64),
        "has_accessibility_snapshot": bool(accessibility_snapshot),
        "link_count": len(links or []),
        "namespace": SPECTRA_NAMESPACE,
    }

    # Store main content
    fact_id = _engram_store(text, metadata, namespace=SPECTRA_NAMESPACE)

    # Store thumbnail separately if we have one
    if thumbnail_b64:
        thumb_meta = {
            "url": url,
            "title": title,
            "timestamp": timestamp,
            "type": "thumbnail",
            "parent_fact_id": fact_id,
            "dedup_key": dedup_key,
        }
        _engram_store(f"[screenshot thumbnail] {url}: {title}", thumb_meta,
                      namespace=SPECTRA_NAMESPACE)

    # Store accessibility snapshot
    if accessibility_snapshot:
        snap_text = json.dumps(accessibility_snapshot, indent=2)[:4000]
        snap_meta = {
            "url": url,
            "title": title,
            "timestamp": timestamp,
            "type": "accessibility_snapshot",
            "parent_fact_id": fact_id,
        }
        _engram_store(snap_text, snap_meta, namespace=SPECTRA_SNAPSHOT_NS)

    # Store link graph
    if links:
        link_text = "\n".join(f"[{l.get('text','')[:50]}] {l.get('href','')}"
                              for l in links[:100])
        link_meta = {
            "url": url,
            "timestamp": timestamp,
            "type": "link_graph",
            "link_count": len(links),
            "parent_fact_id": fact_id,
        }
        _engram_store(link_text, link_meta, namespace=SPECTRA_NAMESPACE)

    return {
        "ok": bool(fact_id),
        "fact_id": fact_id,
        "dedup_key": dedup_key,
        "thumbnail_stored": bool(thumbnail_b64),
    }


def store_feedback(page: str, issues: list) -> dict:
    """Store Soma UX feedback analysis results. (BA.19)"""
    text = json.dumps({"page": page, "issues": issues, "count": len(issues)}, indent=2)
    meta = {
        "page": page,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "issue_count": len(issues),
        "type": "ux_feedback",
    }
    fact_id = _engram_store(text, meta, namespace=SPECTRA_FEEDBACK_NS)
    return {"ok": bool(fact_id), "fact_id": fact_id, "issue_count": len(issues)}


def store_visual_diff(
    page: str,
    diff_score: float,
    diff_image_b64: str,
    changed_regions: list,
    before_url: str = "",
    after_url: str = "",
) -> dict:
    """Store visual diff result for before/after comparison. (BA.14)"""
    text = (
        f"[visual diff] page={page} score={diff_score:.1f} "
        f"regions={len(changed_regions)} before={before_url} after={after_url}"
    )
    meta = {
        "page": page,
        "diff_score": diff_score,
        "changed_regions": changed_regions,
        "before_url": before_url,
        "after_url": after_url,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "type": "visual_diff",
    }
    fact_id = _engram_store(text, meta, namespace=SPECTRA_DIFF_NS)
    return {"ok": bool(fact_id), "fact_id": fact_id, "diff_score": diff_score}


def query_spectra(query: str, source: str = SPECTRA_NAMESPACE, limit: int = 10) -> list:
    """Query stored spectra content from Engram."""
    return _engram_query(query, source=source, limit=limit)
