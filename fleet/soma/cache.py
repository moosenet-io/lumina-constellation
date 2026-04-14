"""
cache.py — Soma caching layer (SP.1)
fleet/soma/cache.py

In-memory TTL cache for all Soma API endpoints that probe external services.
Root cause fix for: slow dashboard, Loading... pages, SSH timeout failures.

Architecture:
- Cache stores display-safe data only (no secrets, tokens, passwords)
- HMAC integrity verification on reads (prevents cache poisoning)
- Background asyncio task refreshes all registered endpoints on their TTL schedule
- On cache miss: synchronous fetch, populate, return
- /api/cache/clear for manual invalidation
- /api/cache/status for debugging

Usage in main.py:
    from cache import SomaCache
    soma_cache = SomaCache(admin_token=SOMA_KEY)
    soma_cache.register('/api/status', refresh_status, ttl=10)
    soma_cache.start_background_refresh(app)  # uses FastAPI lifespan

    @app.get('/api/status')
    async def status(x_soma_key: str = Header(default="")):
        _auth(x_soma_key)
        return soma_cache.get('/api/status') or await refresh_status()
"""

import asyncio
import hashlib
import hmac
import json
import time
from typing import Any, Callable, Dict, Optional

# Sensitive field names — NEVER cache values for these keys
_SENSITIVE_FIELDS = frozenset({
    'token', 'password', 'secret', 'api_key', 'apikey',
    'auth', 'key', 'credential', 'private_key', 'access_token',
    'refresh_token', 'client_secret',
})


def _scrub(data: Any) -> Any:
    """Recursively remove sensitive fields from data before caching."""
    if isinstance(data, dict):
        return {
            k: '[REDACTED]' if k.lower() in _SENSITIVE_FIELDS else _scrub(v)
            for k, v in data.items()
        }
    if isinstance(data, list):
        return [_scrub(item) for item in data]
    return data


class CacheEntry:
    __slots__ = ('value', 'stored_at', 'source_hash', 'hmac_sig')

    def __init__(self, value: Any, hmac_key: bytes):
        self.value = value
        self.stored_at = time.time()
        serialized = json.dumps(value, sort_keys=True, default=str).encode()
        self.source_hash = hashlib.sha256(serialized).hexdigest()
        self.hmac_sig = hmac.new(hmac_key, serialized, hashlib.sha256).hexdigest()

    def is_valid(self, hmac_key: bytes) -> bool:
        """Verify HMAC integrity before returning cached data."""
        try:
            serialized = json.dumps(self.value, sort_keys=True, default=str).encode()
            expected = hmac.new(hmac_key, serialized, hashlib.sha256).hexdigest()
            return hmac.compare_digest(self.hmac_sig, expected)
        except Exception:
            return False

    def age(self) -> float:
        return time.time() - self.stored_at


class _RefresherSpec:
    __slots__ = ('fn', 'ttl', 'last_refresh', 'hits', 'misses')

    def __init__(self, fn: Callable, ttl: int):
        self.fn = fn
        self.ttl = ttl
        self.last_refresh: float = 0
        self.hits: int = 0
        self.misses: int = 0


class SomaCache:
    """
    Central cache for Soma API endpoints.

    Default TTLs (from spec):
      status=10s, config=60s, skills=60s, plugins=60s,
      timers=30s, logs=5s, sessions=30s, vector=10s
    """

    DEFAULT_TTLS = {
        '/api/status':   10,
        '/api/config':   60,
        '/api/skills':   60,
        '/api/plugins':  60,
        '/api/timers':   30,
        '/api/logs':      5,
        '/api/sessions': 30,
        '/api/vector':   10,
        '/api/modules':  60,
        '/api/cost':     30,
    }

    def __init__(self, admin_token: str = 'soma-dev-key'):
        self._hmac_key = admin_token.encode('utf-8') or b'fallback-key'
        self._store: Dict[str, CacheEntry] = {}
        self._refreshers: Dict[str, _RefresherSpec] = {}
        self._lock = asyncio.Lock()

    def register(self, key: str, fn: Callable, ttl: Optional[int] = None):
        """Register a refresh function for a cache key."""
        actual_ttl = ttl or self.DEFAULT_TTLS.get(key, 30)
        self._refreshers[key] = _RefresherSpec(fn, actual_ttl)

    def get(self, key: str) -> Optional[Any]:
        """Return cached value if present and valid. Returns None on miss."""
        entry = self._store.get(key)
        if entry is None:
            spec = self._refreshers.get(key)
            if spec:
                spec.misses += 1
            return None

        # Verify HMAC integrity
        if not entry.is_valid(self._hmac_key):
            del self._store[key]
            return None

        spec = self._refreshers.get(key)
        if spec:
            if entry.age() > spec.ttl:
                spec.misses += 1
                return None
            spec.hits += 1

        return entry.value

    def set(self, key: str, value: Any):
        """Store a value in cache after scrubbing sensitive fields."""
        safe_value = _scrub(value)
        self._store[key] = CacheEntry(safe_value, self._hmac_key)

    def invalidate(self, key: Optional[str] = None):
        """Invalidate one key or all keys."""
        if key:
            self._store.pop(key, None)
        else:
            self._store.clear()

    async def get_or_fetch(self, key: str) -> Any:
        """
        Return cached value, or synchronously fetch + store + return on miss.
        This is the main entry point for API endpoints.
        """
        cached = self.get(key)
        if cached is not None:
            return cached

        spec = self._refreshers.get(key)
        if not spec:
            return None

        async with self._lock:
            # Double-check after acquiring lock
            cached = self.get(key)
            if cached is not None:
                return cached
            try:
                if asyncio.iscoroutinefunction(spec.fn):
                    value = await spec.fn()
                else:
                    loop = asyncio.get_event_loop()
                    value = await loop.run_in_executor(None, spec.fn)
                self.set(key, value)
                spec.last_refresh = time.time()
                return self.get(key)
            except Exception as e:
                return {'ok': False, 'error': str(e)[:200], 'cached_at': None, 'stale': False}

    def status_report(self) -> dict:
        """Return cache health report for /api/cache/status endpoint."""
        now = time.time()
        keys = {}
        for key, spec in self._refreshers.items():
            entry = self._store.get(key)
            keys[key] = {
                'ttl': spec.ttl,
                'age': round(entry.age(), 1) if entry else None,
                'stale': (entry.age() > spec.ttl) if entry else True,
                'hits': spec.hits,
                'misses': spec.misses,
                'hit_ratio': round(spec.hits / max(spec.hits + spec.misses, 1), 2),
                'last_refresh': round(now - spec.last_refresh, 0) if spec.last_refresh else None,
            }
        return {'keys': keys, 'total_entries': len(self._store), 'uptime': round(now)}

    def start_background_refresh(self, app):
        """Register background refresh task with FastAPI app lifespan."""
        cache_ref = self

        @app.on_event('startup')
        async def _start_refresh_loop():
            asyncio.create_task(cache_ref._refresh_loop())

    async def _refresh_loop(self):
        """Background task: refresh all registered keys on their TTL schedule."""
        while True:
            await asyncio.sleep(1)
            now = time.time()
            for key, spec in list(self._refreshers.items()):
                entry = self._store.get(key)
                age = entry.age() if entry else float('inf')
                if age >= spec.ttl * 0.9:  # refresh at 90% of TTL
                    try:
                        if asyncio.iscoroutinefunction(spec.fn):
                            value = await spec.fn()
                        else:
                            loop = asyncio.get_event_loop()
                            value = await loop.run_in_executor(None, spec.fn)
                        existing = self._store.get(key)
                        new_hash = hashlib.sha256(
                            json.dumps(value, sort_keys=True, default=str).encode()
                        ).hexdigest()
                        if existing and existing.source_hash == new_hash:
                            # No change — just bump the timestamp
                            existing.stored_at = now
                        else:
                            self.set(key, value)
                        spec.last_refresh = now
                    except Exception:
                        pass  # Keep stale data, try again next cycle
