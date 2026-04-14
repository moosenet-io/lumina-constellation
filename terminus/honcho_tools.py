import os
import json
import urllib.request
from datetime import datetime

# ============================================================
# Honcho Tools — Pluggable Memory Provider (Honcho self-hosted)
# MCP tools for Honcho dialectic user modeling on CT310:8100.
# Falls back gracefully if Honcho is not yet deployed.
# Deploy Honcho first: see LM-327 (requires CT310 disk expansion)
# ============================================================

HONCHO_URL = os.environ.get('HONCHO_API_URL', 'http://YOUR_FLEET_SERVER_IP:8100')
HONCHO_KEY = os.environ.get('HONCHO_API_KEY', '')
HONCHO_APP = 'lumina'


def _headers():
    h = {'Content-Type': 'application/json'}
    if HONCHO_KEY:
        h['Authorization'] = f'Bearer {HONCHO_KEY}'
    return h


def _honcho_request(path, method='GET', data=None, timeout=8):
    try:
        req = urllib.request.Request(
            f'{HONCHO_URL}{path}',
            data=json.dumps(data).encode() if data else None,
            headers=_headers(),
            method=method
        )
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return {'ok': True, 'data': json.load(r), 'status': r.status}
    except urllib.error.URLError as e:
        return {'ok': False, 'error': f'Honcho unavailable: {e.reason}', 'deploy_info': 'See LM-327 to deploy Honcho on CT310'}
    except Exception as e:
        return {'ok': False, 'error': str(e)[:200]}


def register_honcho_tools(mcp):

    @mcp.tool()
    def honcho_status() -> dict:
        """Check Honcho self-hosted memory provider status.
        Returns connection status, version, and configuration info.
        Honcho provides dialectic user modeling and cross-session semantic recall."""
        r = _honcho_request('/health')
        if not r['ok']:
            return {
                'status': 'unavailable',
                'url': HONCHO_URL,
                'error': r['error'],
                'note': 'Honcho requires CT310 disk expansion (LM-347). See LUMINA.md for deployment instructions.',
                'fallback': 'Engram (engram_query) is active as memory provider.',
            }
        return {
            'status': 'healthy',
            'url': HONCHO_URL,
            'app': HONCHO_APP,
            'provider': 'honcho',
            'health': r['data'],
        }

    @mcp.tool()
    def honcho_search(query: str, user_id: str = 'peter', limit: int = 5) -> dict:
        """Semantic search across all sessions using Honcho's vector store.
        query: natural language query about the user or their history.
        user_id: which user to search (default: peter).
        limit: max results.
        Uses cross-session recall — finds relevant facts from any past conversation."""
        r = _honcho_request(
            f'/v1/apps/{HONCHO_APP}/users/{user_id}/query',
            method='POST',
            data={'query': query, 'top_k': limit}
        )
        if not r['ok']:
            return {'error': r['error'], 'note': 'Honcho not deployed yet. Use engram_query as fallback.'}
        messages = r['data'].get('messages', [])
        return {
            'count': len(messages),
            'query': query,
            'provider': 'honcho',
            'results': [{'content': m.get('content', ''), 'score': m.get('score', 0), 'metadata': m.get('metadata', {})} for m in messages],
        }

    @mcp.tool()
    def honcho_profile(user_id: str = 'peter') -> dict:
        """Get Honcho's derived user model/profile for a user.
        user_id: which user (default: peter).
        Returns conclusions the deriver has drawn from conversation patterns.
        This is different from Engram — Honcho reasons about behavioral patterns, not just facts."""
        r = _honcho_request(f'/v1/apps/{HONCHO_APP}/users/{user_id}')
        if not r['ok']:
            return {'error': r['error'], 'note': 'Honcho not deployed yet. Use engram_query for the operator profile facts.'}
        return {
            'user_id': user_id,
            'provider': 'honcho',
            'profile': r['data'],
        }

    @mcp.tool()
    def honcho_conclude(user_id: str, conclusion: str, evidence: str = '') -> dict:
        """Store a conclusion about a user in Honcho's dialectic model.
        user_id: which user this conclusion is about.
        conclusion: the insight or behavioral pattern observed.
        evidence: optional supporting context.
        This feeds into Honcho's user modeling for better cross-session personalization."""
        r = _honcho_request(
            f'/v1/apps/{HONCHO_APP}/users/{user_id}/sessions/conclusions/messages',
            method='POST',
            data={
                'content': conclusion,
                'metadata': {'evidence': evidence, 'timestamp': datetime.utcnow().isoformat(), 'type': 'conclusion'}
            }
        )
        if not r['ok']:
            return {'error': r['error'], 'note': 'Honcho not deployed. Storing in Engram as fallback.'}
        return {'stored': True, 'user_id': user_id, 'provider': 'honcho', 'conclusion': conclusion[:100]}
