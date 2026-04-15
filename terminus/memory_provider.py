# /opt/ai-mcp/memory_provider.py
"""
Pluggable memory provider interface for Lumina Constellation.
Engram is the default provider. Honcho is an optional alternative.

Usage:
    from memory_provider import get_provider
    provider = get_provider()  # returns configured provider
    provider.store('key', 'value', namespace='agents/lumina')
    results = provider.query('what is the operator's job', namespace='agents/peter')
"""
import os
from abc import ABC, abstractmethod
from typing import Optional

MEMORY_PROVIDER = os.environ.get('MEMORY_PROVIDER', 'engram')


class MemoryProvider(ABC):
    """Abstract base class for memory providers."""

    @abstractmethod
    def store(self, key: str, value: str, namespace: str = '', layer: str = 'kb') -> bool:
        """Store a fact/memory.
        key: hierarchical key (e.g. 'agents/peter/profile/identity')
        value: content to store
        namespace: optional grouping (provider-specific)
        layer: storage tier ('kb', 'episodic', 'working')
        Returns True on success."""

    @abstractmethod
    def query(self, query_text: str, namespace: str = '', limit: int = 5) -> list[dict]:
        """Semantic query for relevant memories.
        query_text: natural language query
        namespace: optional filter by namespace
        limit: max results
        Returns list of dicts: [{key, content, score}]"""

    @abstractmethod
    def list_namespaces(self) -> list[str]:
        """Return all available namespaces/keys."""

    @abstractmethod
    def get_user_profile(self, user_id: str = 'peter') -> dict:
        """Get structured user profile for a given user.
        Returns dict with profile fields from stored memories."""

    @property
    def provider_name(self) -> str:
        return self.__class__.__name__


class EngramProvider(MemoryProvider):
    """Default memory provider using Engram (sqlite-vec on fleet-host)."""

    def __init__(self):
        self._engram = None

    def _get_engram(self):
        if self._engram is None:
            import sys
            sys.path.insert(0, '/opt/ai-mcp')
            import engram_bridge
            self._engram = engram_bridge
        return self._engram

    def store(self, key: str, value: str, namespace: str = '', layer: str = 'kb') -> bool:
        try:
            e = self._get_engram()
            return e.store(key=key, content=value, layer=layer)
        except Exception:
            return False

    def query(self, query_text: str, namespace: str = '', limit: int = 5) -> list[dict]:
        try:
            e = self._get_engram()
            results = e.query(query_text, top_k=limit)
            return results if isinstance(results, list) else []
        except Exception:
            return []

    def list_namespaces(self) -> list[str]:
        try:
            e = self._get_engram()
            return e.list_keys()
        except Exception:
            return []

    def get_user_profile(self, user_id: str = 'peter') -> dict:
        try:
            results = self.query(f'{user_id} profile identity role', limit=10)
            profile = {}
            for r in results:
                key = r.get('key', '')
                if f'/{user_id}/' in key or key.startswith(f'agents/{user_id}'):
                    profile[key] = r.get('content', '')
            return {'user_id': user_id, 'provider': 'engram', 'facts': profile}
        except Exception:
            return {'user_id': user_id, 'provider': 'engram', 'facts': {}}

    @property
    def provider_name(self) -> str:
        return 'engram'


class HonchoProvider(MemoryProvider):
    """Optional memory provider using Honcho self-hosted (fleet-host:8100)."""

    def __init__(self):
        self.base_url = os.environ.get('HONCHO_API_URL', 'http://YOUR_FLEET_SERVER_IP:8100')
        self.api_key = os.environ.get('HONCHO_API_KEY', '')
        self.app_name = 'lumina'
        self._fallback = EngramProvider()

    def _headers(self):
        h = {'Content-Type': 'application/json'}
        if self.api_key:
            h['Authorization'] = f'Bearer {self.api_key}'
        return h

    def _available(self) -> bool:
        try:
            import urllib.request
            req = urllib.request.Request(f'{self.base_url}/health', headers=self._headers())
            with urllib.request.urlopen(req, timeout=3) as r:
                return r.status == 200
        except Exception:
            return False

    def store(self, key: str, value: str, namespace: str = '', layer: str = 'kb') -> bool:
        if not self._available():
            return self._fallback.store(key, value, namespace, layer)
        try:
            import urllib.request, json
            user_id = namespace.split('/')[1] if '/' in namespace else 'lumina'
            payload = json.dumps({
                'content': value,
                'metadata': {'key': key, 'layer': layer}
            }).encode()
            req = urllib.request.Request(
                f'{self.base_url}/v1/apps/{self.app_name}/users/{user_id}/sessions/default/messages',
                data=payload, headers=self._headers(), method='POST'
            )
            with urllib.request.urlopen(req, timeout=10) as r:
                return r.status in (200, 201)
        except Exception:
            return self._fallback.store(key, value, namespace, layer)

    def query(self, query_text: str, namespace: str = '', limit: int = 5) -> list[dict]:
        if not self._available():
            return self._fallback.query(query_text, namespace, limit)
        try:
            import urllib.request, json
            user_id = namespace.split('/')[1] if '/' in namespace else 'lumina'
            payload = json.dumps({'query': query_text, 'top_k': limit}).encode()
            req = urllib.request.Request(
                f'{self.base_url}/v1/apps/{self.app_name}/users/{user_id}/query',
                data=payload, headers=self._headers(), method='POST'
            )
            with urllib.request.urlopen(req, timeout=10) as r:
                data = json.load(r)
                return [{'key': m.get('metadata', {}).get('key', ''), 'content': m.get('content', ''), 'score': m.get('score', 0)} for m in data.get('messages', [])]
        except Exception:
            return self._fallback.query(query_text, namespace, limit)

    def list_namespaces(self) -> list[str]:
        return self._fallback.list_namespaces()

    def get_user_profile(self, user_id: str = 'peter') -> dict:
        if not self._available():
            return self._fallback.get_user_profile(user_id)
        try:
            import urllib.request, json
            req = urllib.request.Request(
                f'{self.base_url}/v1/apps/{self.app_name}/users/{user_id}',
                headers=self._headers()
            )
            with urllib.request.urlopen(req, timeout=10) as r:
                data = json.load(r)
                return {'user_id': user_id, 'provider': 'honcho', 'profile': data}
        except Exception:
            return self._fallback.get_user_profile(user_id)

    @property
    def provider_name(self) -> str:
        return 'honcho'


_provider_cache: Optional[MemoryProvider] = None

def get_provider() -> MemoryProvider:
    """Get the configured memory provider (singleton)."""
    global _provider_cache
    if _provider_cache is None:
        provider_name = os.environ.get('MEMORY_PROVIDER', 'engram').lower()
        if provider_name == 'honcho':
            _provider_cache = HonchoProvider()
        else:
            _provider_cache = EngramProvider()
    return _provider_cache
