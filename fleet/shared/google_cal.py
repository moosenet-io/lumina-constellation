"""
google_cal.py — Robust Google Calendar wrapper
MooseNet · Document addenda ADD.2 implementation

Features:
  - Retry with exponential backoff (configurable, default 3 attempts)
  - Classified error types: AUTH_FAILED, NETWORK_ERROR, RATE_LIMITED, NOT_FOUND, UNKNOWN
  - 5-minute in-memory + on-disk cache for event reads
  - Sentinel health check integration (writes to sentinel health state file)
  - Drop-in replacement for direct googleapiclient calls

Usage:
    from google_cal import GoogleCalClient, CalendarError

    client = GoogleCalClient()
    try:
        events = client.list_events(calendar_id='primary', days_ahead=7)
    except CalendarError as e:
        if e.error_type == 'AUTH_FAILED':
            # notify user to re-auth
            pass

Environment variables:
    GOOGLE_CREDENTIALS_PATH  — path to credentials.json (default: ~/.config/google/credentials.json)
    GOOGLE_TOKEN_PATH        — path to token.json (default: ~/.config/google/token.json)
    GOOGLE_CAL_CACHE_TTL     — cache TTL in seconds (default: 300)
    SENTINEL_HEALTH_PATH     — Sentinel health state file for integration
"""

import json
import os
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

# Optional google-auth imports — graceful failure if not installed
try:
    from google.oauth2.credentials import Credentials
    from google.auth.transport.requests import Request
    from google_auth_oauthlib.flow import InstalledAppFlow
    from googleapiclient.discovery import build
    from googleapiclient.errors import HttpError
    _HAS_GOOGLE = True
except ImportError:
    _HAS_GOOGLE = False

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

SCOPES = ["https://www.googleapis.com/auth/calendar.readonly"]

CREDENTIALS_PATH = Path(os.environ.get(
    "GOOGLE_CREDENTIALS_PATH",
    Path.home() / ".config" / "google" / "credentials.json",
))
TOKEN_PATH = Path(os.environ.get(
    "GOOGLE_TOKEN_PATH",
    Path.home() / ".config" / "google" / "token.json",
))
CACHE_TTL = int(os.environ.get("GOOGLE_CAL_CACHE_TTL", "300"))  # 5 min
SENTINEL_HEALTH_PATH = Path(os.environ.get(
    "SENTINEL_HEALTH_PATH",
    "/opt/lumina-fleet/sentinel/health.json",
))

CACHE_FILE = Path(os.environ.get(
    "GOOGLE_CAL_CACHE_PATH",
    "/tmp/google_cal_cache.json",
))

# ---------------------------------------------------------------------------
# Error types
# ---------------------------------------------------------------------------

class CalendarError(Exception):
    """Structured calendar error with classification."""

    TYPES = ("AUTH_FAILED", "NETWORK_ERROR", "RATE_LIMITED", "NOT_FOUND", "UNKNOWN")

    def __init__(self, error_type: str, message: str, original: Exception = None):
        assert error_type in self.TYPES, f"Unknown error type: {error_type}"
        self.error_type = error_type
        self.message = message
        self.original = original
        super().__init__(f"[{error_type}] {message}")


def _classify_http_error(e) -> CalendarError:
    """Map HttpError status code to CalendarError type."""
    status = getattr(e, "resp", {}).get("status", "") if hasattr(e, "resp") else ""
    try:
        status = int(status)
    except (TypeError, ValueError):
        status = 0

    if status in (401, 403):
        return CalendarError("AUTH_FAILED", f"HTTP {status}: {e}", e)
    elif status == 429:
        return CalendarError("RATE_LIMITED", f"HTTP 429 rate limited: {e}", e)
    elif status == 404:
        return CalendarError("NOT_FOUND", f"HTTP 404: {e}", e)
    elif status >= 500:
        return CalendarError("NETWORK_ERROR", f"HTTP {status} server error: {e}", e)
    return CalendarError("UNKNOWN", str(e), e)


# ---------------------------------------------------------------------------
# Cache
# ---------------------------------------------------------------------------

_MEMORY_CACHE: dict[str, tuple[float, Any]] = {}  # key → (expiry_ts, data)


def _cache_get(key: str) -> Any | None:
    """Check memory cache, then disk cache."""
    now = time.time()
    if key in _MEMORY_CACHE:
        expiry, data = _MEMORY_CACHE[key]
        if now < expiry:
            return data
        del _MEMORY_CACHE[key]

    if CACHE_FILE.exists():
        try:
            with open(CACHE_FILE) as f:
                disk = json.load(f)
            if key in disk:
                expiry, data = disk[key]["expiry"], disk[key]["data"]
                if now < expiry:
                    _MEMORY_CACHE[key] = (expiry, data)
                    return data
        except Exception:
            pass
    return None


def _cache_set(key: str, data: Any, ttl: int = CACHE_TTL):
    """Write to memory and disk cache."""
    expiry = time.time() + ttl
    _MEMORY_CACHE[key] = (expiry, data)
    try:
        disk = {}
        if CACHE_FILE.exists():
            with open(CACHE_FILE) as f:
                disk = json.load(f)
        disk[key] = {"expiry": expiry, "data": data}
        # Prune expired
        now = time.time()
        disk = {k: v for k, v in disk.items() if v["expiry"] > now}
        with open(CACHE_FILE, "w") as f:
            json.dump(disk, f)
    except Exception:
        pass


def cache_clear():
    """Clear all cached calendar data."""
    _MEMORY_CACHE.clear()
    if CACHE_FILE.exists():
        CACHE_FILE.unlink()


# ---------------------------------------------------------------------------
# Sentinel integration
# ---------------------------------------------------------------------------

def _sentinel_update(component: str, healthy: bool, message: str = ""):
    """Write component health to Sentinel's health state file."""
    try:
        SENTINEL_HEALTH_PATH.parent.mkdir(parents=True, exist_ok=True)
        data = {}
        if SENTINEL_HEALTH_PATH.exists():
            with open(SENTINEL_HEALTH_PATH) as f:
                data = json.load(f)
        data[component] = {
            "healthy": healthy,
            "message": message,
            "last_check": datetime.now(timezone.utc).isoformat(),
        }
        with open(SENTINEL_HEALTH_PATH, "w") as f:
            json.dump(data, f, indent=2)
    except Exception:
        pass  # Never fail due to Sentinel reporting


# ---------------------------------------------------------------------------
# Retry decorator
# ---------------------------------------------------------------------------

def _with_retry(func, max_attempts: int = 3, base_delay: float = 1.0):
    """
    Call func with exponential backoff. Returns result or raises CalendarError.
    Does not retry AUTH_FAILED or NOT_FOUND — those won't fix themselves.
    """
    last_error = None
    for attempt in range(max_attempts):
        try:
            return func()
        except CalendarError as e:
            last_error = e
            if e.error_type in ("AUTH_FAILED", "NOT_FOUND"):
                raise  # Non-retriable
            if attempt < max_attempts - 1:
                delay = base_delay * (2 ** attempt)
                time.sleep(delay)
        except Exception as e:
            last_error = CalendarError("UNKNOWN", str(e), e)
            if attempt < max_attempts - 1:
                time.sleep(base_delay * (2 ** attempt))

    raise last_error


# ---------------------------------------------------------------------------
# Client
# ---------------------------------------------------------------------------

class GoogleCalClient:
    """
    Robust Google Calendar API client with retry, caching, and Sentinel integration.
    """

    def __init__(self, max_retries: int = 3, cache_ttl: int = CACHE_TTL):
        if not _HAS_GOOGLE:
            raise ImportError(
                "google-auth / google-api-python-client not installed. "
                "Run: pip install google-auth google-auth-oauthlib google-api-python-client"
            )
        self.max_retries = max_retries
        self.cache_ttl = cache_ttl
        self._service = None

    def _get_service(self):
        """Build or return cached Google Calendar service."""
        if self._service:
            return self._service

        creds = None
        if TOKEN_PATH.exists():
            try:
                creds = Credentials.from_authorized_user_file(str(TOKEN_PATH), SCOPES)
            except Exception as e:
                raise CalendarError("AUTH_FAILED", f"Failed to load token: {e}", e)

        if not creds or not creds.valid:
            if creds and creds.expired and creds.refresh_token:
                try:
                    creds.refresh(Request())
                    with open(TOKEN_PATH, "w") as f:
                        f.write(creds.to_json())
                except Exception as e:
                    raise CalendarError("AUTH_FAILED", f"Token refresh failed: {e}", e)
            else:
                raise CalendarError(
                    "AUTH_FAILED",
                    "No valid credentials. Run the auth flow interactively first.",
                )

        try:
            self._service = build("calendar", "v3", credentials=creds)
            _sentinel_update("google_calendar", True, "Auth OK")
            return self._service
        except Exception as e:
            raise CalendarError("UNKNOWN", f"Failed to build Calendar service: {e}", e)

    def list_events(
        self,
        calendar_id: str = "primary",
        days_ahead: int = 7,
        max_results: int = 50,
        use_cache: bool = True,
    ) -> list[dict]:
        """
        List upcoming events within days_ahead days.
        Returns list of simplified event dicts.
        Raises CalendarError on failure.
        """
        cache_key = f"events:{calendar_id}:{days_ahead}:{max_results}"
        if use_cache:
            cached = _cache_get(cache_key)
            if cached is not None:
                return cached

        def _fetch():
            service = self._get_service()
            now_utc = datetime.now(timezone.utc)
            time_min = now_utc.isoformat()
            time_max = (now_utc + timedelta(days=days_ahead)).isoformat()
            try:
                result = service.events().list(
                    calendarId=calendar_id,
                    timeMin=time_min,
                    timeMax=time_max,
                    maxResults=max_results,
                    singleEvents=True,
                    orderBy="startTime",
                ).execute()
                return result.get("items", [])
            except HttpError as e:
                raise _classify_http_error(e)

        try:
            events = _with_retry(_fetch, max_attempts=self.max_retries)
            simplified = [self._simplify_event(e) for e in events]
            _cache_set(cache_key, simplified, self.cache_ttl)
            _sentinel_update("google_calendar", True, f"Fetched {len(simplified)} events")
            return simplified
        except CalendarError as e:
            _sentinel_update("google_calendar", False, str(e))
            raise

    def get_todays_events(self, calendar_id: str = "primary") -> list[dict]:
        """Convenience: today's events only."""
        events = self.list_events(calendar_id=calendar_id, days_ahead=1)
        today = datetime.now().date()
        return [
            e for e in events
            if self._event_date(e) == today
        ]

    def has_free_window(self, duration_minutes: int = 60) -> bool:
        """
        Returns True if there is a gap of >= duration_minutes in today's calendar.
        Pure Python, $0.
        """
        try:
            events = self.get_todays_events()
        except CalendarError:
            return False

        now = datetime.now()
        end_of_day = now.replace(hour=22, minute=0, second=0, microsecond=0)
        busy_slots = []

        for e in events:
            start = e.get("start_dt")
            end = e.get("end_dt")
            if start and end:
                try:
                    s = datetime.fromisoformat(start.replace("Z", "+00:00")).replace(tzinfo=None)
                    en = datetime.fromisoformat(end.replace("Z", "+00:00")).replace(tzinfo=None)
                    busy_slots.append((s, en))
                except Exception:
                    pass

        busy_slots.sort()
        cursor = now
        for s, e in busy_slots:
            if (s - cursor).total_seconds() >= duration_minutes * 60:
                return True
            cursor = max(cursor, e)

        return (end_of_day - cursor).total_seconds() >= duration_minutes * 60

    @staticmethod
    def _simplify_event(event: dict) -> dict:
        """Extract key fields from raw Google Calendar event."""
        start = event.get("start", {})
        end = event.get("end", {})
        return {
            "id": event.get("id", ""),
            "title": event.get("summary", "(no title)"),
            "start_dt": start.get("dateTime", start.get("date", "")),
            "end_dt": end.get("dateTime", end.get("date", "")),
            "all_day": "date" in start and "dateTime" not in start,
            "location": event.get("location", ""),
            "description": event.get("description", "")[:200],
            "status": event.get("status", "confirmed"),
        }

    @staticmethod
    def _event_date(event: dict):
        """Extract date from simplified event dict."""
        dt_str = event.get("start_dt", "")
        try:
            return datetime.fromisoformat(dt_str.replace("Z", "+00:00")).date()
        except Exception:
            try:
                return datetime.strptime(dt_str[:10], "%Y-%m-%d").date()
            except Exception:
                return None
