"""
Vector Engram MemoryStore — psycopg2 backend to Engram via shared Nexus DB.
Falls back to local file-based memory if Engram DB unavailable.
"""
import os
import json
import logging
import urllib.request
from pathlib import Path
from datetime import datetime

log = logging.getLogger('vector.memory.engram')

ENGRAM_DB_PATH = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
EMBED_URL = os.environ.get('REFLEXA_EMBED_URL', 'http://YOUR_LITELLM_IP:4000/v1/embeddings')
EMBED_MODEL = os.environ.get('REFLEXA_EMBED_MODEL', 'text-embedding')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')


class EngramMemoryStore:
    """MemoryStore backed by Engram's sqlite-vec database."""

    def __init__(self, fallback_dir: str = './memory', conventions_file: str = './conventions.md'):
        self.fallback_dir = Path(fallback_dir)
        self.conventions_file = Path(conventions_file)
        self._db = None
        self._available = False
        self._try_connect()

    def _try_connect(self):
        try:
            import sqlite3
            import sqlite_vec
            if not Path(ENGRAM_DB_PATH).exists():
                log.warning(f'Engram DB not found at {ENGRAM_DB_PATH}, falling back to local memory')
                return
            conn = sqlite3.connect(ENGRAM_DB_PATH)
            conn.enable_load_extension(True)
            sqlite_vec.load(conn)
            conn.enable_load_extension(False)
            self._db = conn
            self._available = True
            log.info('EngramMemoryStore connected to sqlite-vec DB')
        except Exception as e:
            log.warning(f'Engram DB unavailable: {e}, using local fallback')

    def _embed(self, text: str) -> list:
        """Get embedding vector from LiteLLM proxy."""
        try:
            data = json.dumps({'model': EMBED_MODEL, 'input': text[:6000]}).encode()
            req = urllib.request.Request(
                EMBED_URL, data=data,
                headers={'Authorization': f'Bearer {LITELLM_KEY}', 'Content-Type': 'application/json'},
                method='POST'
            )
            with urllib.request.urlopen(req, timeout=15) as r:
                return json.load(r)['data'][0]['embedding']
        except Exception as e:
            log.warning(f'Embedding failed: {e}')
            return []

    def query(self, query: str, top_k: int = 5) -> list:
        """Semantic similarity search across Engram DB."""
        if not self._available:
            return self._local_query(query, top_k)
        try:
            import struct
            embedding = self._embed(query)
            if not embedding:
                return self._local_query(query, top_k)
            dim = len(embedding)
            vec_bytes = struct.pack(f'{dim}f', *embedding)
            cur = self._db.cursor()
            cur.execute("""
                SELECT m.key, m.content, vec_distance_cosine(e.embedding, ?) as dist
                FROM memories m
                JOIN memory_embeddings e ON m.id = e.memory_id
                WHERE m.layer IN ('kb', 'patterns', 'journal')
                ORDER BY dist ASC LIMIT ?
            """, (vec_bytes, top_k))
            rows = cur.fetchall()
            return [r[1] for r in rows if r[1]]
        except Exception as e:
            log.warning(f'Engram query failed: {e}')
            return self._local_query(query, top_k)

    def store(self, key: str, content: str, layer: str = 'journal', agent_id: str = 'vector') -> bool:
        """Store a memory in Engram."""
        if not self._available:
            return self._local_store(key, content)
        try:
            import struct
            embedding = self._embed(content)
            cur = self._db.cursor()
            cur.execute("""
                INSERT OR REPLACE INTO memories (key, content, layer, agent_id, namespace, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?)
            """, (key, content, layer, agent_id, f'agents/{agent_id}', datetime.utcnow().isoformat(), datetime.utcnow().isoformat()))
            mem_id = cur.lastrowid
            if embedding:
                dim = len(embedding)
                vec_bytes = struct.pack(f'{dim}f', *embedding)
                cur.execute("INSERT OR REPLACE INTO memory_embeddings (memory_id, embedding) VALUES (?, ?)",
                            (mem_id, vec_bytes))
            self._db.commit()
            return True
        except Exception as e:
            log.warning(f'Engram store failed: {e}')
            return self._local_store(key, content)

    def get_conventions(self) -> str:
        """Get coding conventions — from file if available, else Engram."""
        if self.conventions_file.exists():
            return self.conventions_file.read_text()[:2000]
        if self._available:
            try:
                cur = self._db.cursor()
                cur.execute("SELECT content FROM memories WHERE key LIKE '%conventions%' ORDER BY updated_at DESC LIMIT 1")
                row = cur.fetchone()
                if row:
                    return row[0][:2000]
            except Exception:
                pass
        return 'python3, snake_case, type hints, minimal dependencies'

    # ── Local fallback ──────────────────────────────────────────────────────

    def _local_query(self, query: str, top_k: int) -> list:
        """Simple keyword search through local memory files."""
        self.fallback_dir.mkdir(parents=True, exist_ok=True)
        results = []
        query_lower = query.lower()
        for f in sorted(self.fallback_dir.glob('*.txt'), key=lambda x: x.stat().st_mtime, reverse=True)[:20]:
            content = f.read_text()
            if any(word in content.lower() for word in query_lower.split()[:3]):
                results.append(content[:300])
                if len(results) >= top_k:
                    break
        return results

    def _local_store(self, key: str, content: str) -> bool:
        """Store to local file."""
        try:
            self.fallback_dir.mkdir(parents=True, exist_ok=True)
            safe_key = key.replace('/', '_').replace(' ', '-')[:60]
            (self.fallback_dir / f'{safe_key}.txt').write_text(content)
            return True
        except Exception as e:
            log.warning(f'Local store failed: {e}')
            return False
