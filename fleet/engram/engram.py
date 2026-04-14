#!/usr/bin/env python3
"""
Engram — Semantic memory system for Lumina agents.
Three layers: knowledge base (facts), activity journal (events), patterns (conventions).
Uses sqlite-vec for embedding storage and cosine similarity search.
CT310 /opt/lumina-fleet/engram/engram.py
"""

import os
import sys
import json
import re
import sqlite3
import sqlite_vec
import urllib.request
import argparse
from pathlib import Path
from datetime import datetime
from typing import Optional, List

DB_PATH = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
EMBED_URL = os.environ.get('REFLEXA_EMBED_URL', 'http://YOUR_LITELLM_IP:4000/v1/embeddings')
EMBED_MODEL = os.environ.get('REFLEXA_EMBED_MODEL', 'text-embedding')
EMBED_DIM = int(os.environ.get('REFLEXA_EMBED_DIM', '1536'))
LLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
GITEA_URL = os.environ.get('GITEA_URL', 'http://YOUR_GITEA_IP:3000')
GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')
TOP_K = int(os.environ.get('REFLEXA_TOP_K', '5'))


# ── ENG-65/66/67 additions ────────────────────────────────────────────────────

STOPWORDS = {
    'a', 'an', 'the', 'is', 'are', 'was', 'were', 'be', 'been', 'being',
    'have', 'has', 'had', 'do', 'does', 'did', 'will', 'would', 'could',
    'should', 'may', 'might', 'shall', 'can', 'need', 'dare', 'ought',
    'used', 'to', 'of', 'in', 'for', 'on', 'with', 'at', 'by', 'from',
    'up', 'about', 'into', 'through', 'during', 'and', 'but', 'or', 'nor',
    'not', 'so', 'yet', 'both', 'either', 'neither', 'each', 'few', 'more',
    'most', 'other', 'some', 'such', 'than', 'too', 'very', 'just', 'that',
    'this', 'it', 'its', 'as', 'if', 'when', 'where', 'which', 'who', 'whom',
    'i', 'me', 'my', 'we', 'our', 'you', 'your', 'he', 'she', 'they', 'them',
    'his', 'her', 'their', 'what', 'how', 'all', 'also', 'no', 'any', 'same',
}


def _extract_keywords(text: str, max_keywords: int = 10) -> List[str]:
    """Extract meaningful keywords from text. Pure Python, no LLM."""
    words = re.findall(r'\b[a-zA-Z][a-zA-Z0-9_-]{2,}\b', text.lower())
    seen = set()
    keywords = []
    for w in words:
        if w not in STOPWORDS and w not in seen:
            seen.add(w)
            keywords.append(w)
    return keywords[:max_keywords]


def _migrate_schema(conn):
    """Add Zettelkasten columns and tables if not present. Idempotent.

    SQLite ALTER TABLE only allows constant defaults (no function calls).
    Timestamps default to NULL here; store() sets them explicitly.
    """
    existing = {row[1] for row in conn.execute("PRAGMA table_info(knowledge_base)").fetchall()}
    if 'note_id' not in existing:
        conn.execute("ALTER TABLE knowledge_base ADD COLUMN note_id TEXT")
    if 'keywords' not in existing:
        conn.execute("ALTER TABLE knowledge_base ADD COLUMN keywords TEXT DEFAULT '[]'")
    if 'source_agent' not in existing:
        conn.execute("ALTER TABLE knowledge_base ADD COLUMN source_agent TEXT DEFAULT 'lumina'")
    if 'created_at' not in existing:
        conn.execute("ALTER TABLE knowledge_base ADD COLUMN created_at TEXT")
    if 'updated_at' not in existing:
        conn.execute("ALTER TABLE knowledge_base ADD COLUMN updated_at TEXT")
    if 'version' not in existing:
        conn.execute("ALTER TABLE knowledge_base ADD COLUMN version INTEGER DEFAULT 1")
    if 'discarded' not in existing:
        conn.execute("ALTER TABLE knowledge_base ADD COLUMN discarded INTEGER DEFAULT 0")

    conn.execute("""
        CREATE TABLE IF NOT EXISTS memory_links (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            note_id_1 TEXT NOT NULL,
            note_id_2 TEXT NOT NULL,
            link_strength INTEGER DEFAULT 1,
            link_type TEXT DEFAULT 'related',
            created_at TEXT DEFAULT (datetime('now')),
            UNIQUE(note_id_1, note_id_2)
        )
    """)
    conn.commit()


def _next_note_id(conn) -> str:
    """Generate next ENG-NNNN note_id."""
    row = conn.execute(
        "SELECT note_id FROM knowledge_base WHERE note_id IS NOT NULL ORDER BY note_id DESC LIMIT 1"
    ).fetchone()
    if row and row['note_id']:
        try:
            seq = int(row['note_id'].split('-')[1]) + 1
        except (IndexError, ValueError):
            seq = 1
    else:
        seq = 1
    return f'ENG-{seq:04d}'


def _create_links(conn, new_note_id: str, new_keywords: List[str], min_overlap: int = 2) -> int:
    """Find existing facts with keyword overlap and create bidirectional links."""
    if not new_keywords:
        return 0
    rows = conn.execute(
        "SELECT note_id, keywords FROM knowledge_base WHERE note_id IS NOT NULL AND note_id != ? AND discarded = 0",
        (new_note_id,)
    ).fetchall()
    links_created = 0
    new_kw_set = set(new_keywords)
    for row in rows:
        if not row['note_id'] or not row['keywords']:
            continue
        try:
            existing_kws = set(json.loads(row['keywords']))
        except Exception:
            continue
        overlap = len(new_kw_set & existing_kws)
        if overlap >= min_overlap:
            try:
                conn.execute(
                    "INSERT OR REPLACE INTO memory_links (note_id_1, note_id_2, link_strength, link_type) "
                    "VALUES (?, ?, ?, 'related')",
                    (min(new_note_id, row['note_id']), max(new_note_id, row['note_id']), overlap)
                )
                links_created += 1
            except Exception:
                pass
    return links_created


def _get_linked_facts(conn, note_ids: List[str], limit: int = 3) -> List[str]:
    """Get facts linked to given note_ids (1 hop)."""
    if not note_ids:
        return []
    placeholders = ','.join('?' * len(note_ids))
    rows = conn.execute(f"""
        SELECT DISTINCT kb.content, kb.key
        FROM memory_links ml
        JOIN knowledge_base kb ON (
            (ml.note_id_1 = kb.note_id AND ml.note_id_2 IN ({placeholders})) OR
            (ml.note_id_2 = kb.note_id AND ml.note_id_1 IN ({placeholders}))
        )
        WHERE kb.discarded = 0
        ORDER BY ml.link_strength DESC
        LIMIT ?
    """, note_ids + note_ids + [limit]).fetchall()
    return [f"[linked:{r['key']}] {r['content']}" for r in rows]


def _detect_contradiction(new_content: str, existing_content: str) -> bool:
    """Simple heuristic contradiction detection. Python only, no LLM."""
    contradiction_patterns = [
        (r'(?i)(\w+)\s+lives?\s+in\s+(\w[\w\s]+)', 'location'),
        (r'(?i)(\w+)\s+works?\s+(?:at|for)\s+(\w[\w\s]+)', 'employer'),
        (r'(?i)(\w+)\s+is\s+(\w+(?:\s+\w+)?)\s*$', 'identity'),
    ]
    for pattern, _ in contradiction_patterns:
        m1 = re.search(pattern, new_content)
        m2 = re.search(pattern, existing_content)
        if m1 and m2:
            if m1.group(1).lower() == m2.group(1).lower() and m1.group(2).lower() != m2.group(2).lower():
                return True
    return False


# ── end ENG-65/66/67 additions ────────────────────────────────────────────────


def _resolve_namespace(key: str, layer: str, agent_id: str = 'lumina') -> str:
    """Resolve storage key to correct namespace based on layer and agent.

    Personal layers: health, learning, trading, work, personal, commute
    → stored at agents/{agent_id}/{layer}/{key}

    Shared layers: grocery, meal-plan, travel, vehicle, finance, calendar
    → stored at household/{layer}/{key}

    System layers: infrastructure, patterns, kb, journal
    → stored at system/{key} (Lumina write, all read)
    """
    personal_layers = {'health', 'learning', 'trading', 'work', 'personal', 'commute'}
    shared_layers = {'grocery', 'meal-plan', 'travel', 'vehicle', 'finance', 'calendar'}

    # Keys that explicitly start with agents/ or household/ are already resolved
    if key.startswith(('agents/', 'household/', 'system/', 'travel/', 'meridian/', 'crucible/')):
        return key

    if layer in personal_layers:
        return f'agents/{agent_id}/{layer}/{key}'
    elif layer in shared_layers:
        return f'household/{layer}/{key}'
    else:
        return key  # kb, patterns, journal stay as-is for now


def _db():
    conn = sqlite3.connect(DB_PATH)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)
    conn.row_factory = sqlite3.Row
    _migrate_schema(conn)
    return conn


def embed(text: str) -> Optional[List[float]]:
    """Get embedding vector for text. Returns None on failure."""
    try:
        data = json.dumps({'model': EMBED_MODEL, 'input': text[:4000]}).encode()
        req = urllib.request.Request(
            EMBED_URL, data=data,
            headers={'Authorization': f'Bearer {LLM_KEY}', 'Content-Type': 'application/json'},
            method='POST')
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.load(r)['data'][0]['embedding']
    except Exception as e:
        print(f'[engram] embed failed: {e}', file=sys.stderr)
        return None


def store(key: str, content: str, layer: str = 'kb', tags: List[str] = None,
          source_agent: str = 'lumina') -> bool:
    """Store a knowledge entry with embedding. Returns True on success."""
    if tags is None:
        tags = []
    embedding = embed(content)

    try:
        conn = _db()
        if layer == 'kb':
            # ENG-66: extract keywords for structured attributes
            keywords = _extract_keywords(content)

            # ENG-65: check if this key already exists (for contradiction detection)
            existing_row = conn.execute(
                'SELECT note_id, content, keywords, version FROM knowledge_base WHERE key = ?', (key,)
            ).fetchone()

            if existing_row:
                # ENG-67: contradiction detection against previous content
                if _detect_contradiction(content, existing_row['content']):
                    if 'needs_review' not in tags:
                        tags = list(tags) + ['needs_review']
                    # Tag the old entry too
                    try:
                        old_tags = json.loads(conn.execute(
                            'SELECT tags FROM knowledge_base WHERE key = ?', (key,)
                        ).fetchone()['tags'] or '[]')
                        if 'needs_review' not in old_tags:
                            old_tags.append('needs_review')
                            conn.execute('UPDATE knowledge_base SET tags = ? WHERE key = ?',
                                         (json.dumps(old_tags), key))
                    except Exception:
                        pass
                    print(f'[engram] contradiction detected for key={key}, tagged needs_review', file=sys.stderr)
                note_id = existing_row['note_id'] or _next_note_id(conn)
                new_version = (existing_row['version'] or 1) + 1
                cur = conn.execute(
                    'UPDATE knowledge_base SET content=?, tags=?, keywords=?, source_agent=?, '
                    'updated_at=datetime("now"), version=? WHERE key=?',
                    (content, json.dumps(tags), json.dumps(keywords), source_agent, new_version, key))
                # Refresh rowid for vec upsert
                rowid = conn.execute('SELECT rowid FROM knowledge_base WHERE key=?', (key,)).fetchone()[0]
            else:
                note_id = _next_note_id(conn)
                cur = conn.execute(
                    'INSERT INTO knowledge_base (key, content, tags, layer, note_id, keywords, source_agent, '
                    'created_at, updated_at, version, discarded) '
                    'VALUES (?,?,?,?,?,?,?,datetime("now"),datetime("now"),1,0)',
                    (key, content, json.dumps(tags), layer, note_id, json.dumps(keywords), source_agent))
                rowid = cur.lastrowid

            if embedding:
                conn.execute('DELETE FROM kb_vec WHERE rowid = ?', (rowid,))
                conn.execute('INSERT INTO kb_vec(rowid, embedding) VALUES (?,?)',
                             (rowid, sqlite_vec.serialize_float32(embedding)))

            # ENG-65: create links to related existing facts
            links = _create_links(conn, note_id, keywords)
            if links:
                print(f'[engram] created {links} link(s) for {note_id}', file=sys.stderr)

        elif layer == 'patterns':
            cur = conn.execute(
                'INSERT OR REPLACE INTO patterns (key, content, tags) VALUES (?,?,?)',
                (key, content, json.dumps(tags)))
            rowid = cur.lastrowid
            if embedding:
                conn.execute('INSERT INTO patterns_vec(rowid, embedding) VALUES (?,?)',
                             (rowid, sqlite_vec.serialize_float32(embedding)))
        conn.commit()
        conn.close()
        return True
    except Exception as e:
        print(f'[engram] store failed: {e}', file=sys.stderr)
        return False


def query(query_text: str, layer: str = None, top_k: int = None) -> List[str]:
    """Semantic search across memory layers. Returns list of relevant content strings."""
    if top_k is None:
        top_k = TOP_K

    embedding = embed(query_text)
    if not embedding:
        return _fallback_search(query_text, layer, top_k)

    results = []
    result_note_ids = []
    emb_blob = sqlite_vec.serialize_float32(embedding)

    try:
        conn = _db()
        if layer in (None, 'kb'):
            rows = conn.execute(
                '''SELECT kb.content, kb.key, kb.note_id, distance
                   FROM kb_vec kv
                   JOIN knowledge_base kb ON kb.rowid = kv.rowid
                   WHERE kv.embedding MATCH ? AND k = ?
                   ORDER BY distance''',
                (emb_blob, top_k)).fetchall()
            results.extend(f"[{r['key']}] {r['content']}" for r in rows)
            result_note_ids.extend(r['note_id'] for r in rows if r['note_id'])

        if layer in (None, 'patterns'):
            rows = conn.execute(
                '''SELECT p.content, p.key, distance
                   FROM patterns_vec pv
                   JOIN patterns p ON p.rowid = pv.rowid
                   WHERE pv.embedding MATCH ? AND k = ?
                   ORDER BY distance''',
                (emb_blob, top_k)).fetchall()
            results.extend(f"[pattern:{r['key']}] {r['content']}" for r in rows)

        # ENG-65: append 1-hop linked facts (deduped, don't exceed top_k total)
        if result_note_ids and layer in (None, 'kb'):
            linked = _get_linked_facts(conn, result_note_ids, limit=3)
            # Deduplicate against already-found results
            existing_snippets = set(results)
            for item in linked:
                if item not in existing_snippets:
                    results.append(item)
                    existing_snippets.add(item)

        conn.close()
    except Exception as e:
        print(f'[engram] query failed, using fallback: {e}', file=sys.stderr)
        return _fallback_search(query_text, layer, top_k)

    return results[:top_k]


def _fallback_search(query_text: str, layer: str, top_k: int) -> List[str]:
    """Text search fallback when embedding fails."""
    keywords = query_text.lower().split()
    results = []
    try:
        conn = _db()
        tables = []
        if layer in (None, 'kb'):
            tables.append(('knowledge_base', 'key', 'content'))
        if layer in (None, 'patterns'):
            tables.append(('patterns', 'key', 'content'))

        for table, key_col, content_col in tables:
            rows = conn.execute(f'SELECT {key_col}, {content_col} FROM {table}').fetchall()
            for row in rows:
                score = sum(1 for kw in keywords if kw in row[1].lower())
                if score > 0:
                    results.append((score, f'[{row[0]}] {row[1]}'))
        conn.close()
    except Exception:
        pass

    results.sort(key=lambda x: -x[0])
    return [r[1] for r in results[:top_k]]


def query_by_tag(tag: str, limit: int = 10) -> List[str]:
    """Return knowledge_base entries that have a specific tag. ENG-66."""
    try:
        conn = _db()
        rows = conn.execute(
            "SELECT key, content, tags FROM knowledge_base WHERE discarded = 0 AND tags LIKE ?",
            (f'%"{tag}"%',)
        ).fetchall()
        conn.close()
        results = []
        for r in rows:
            try:
                if tag in json.loads(r['tags'] or '[]'):
                    results.append(f"[{r['key']}] {r['content']}")
            except Exception:
                pass
        return results[:limit]
    except Exception as e:
        print(f'[engram] query_by_tag failed: {e}', file=sys.stderr)
        return []


def query_by_keyword(keyword: str, limit: int = 10) -> List[str]:
    """Return knowledge_base entries that contain a specific keyword. ENG-66."""
    kw = keyword.lower()
    try:
        conn = _db()
        rows = conn.execute(
            "SELECT key, content, keywords FROM knowledge_base WHERE discarded = 0 AND keywords LIKE ?",
            (f'%"{kw}"%',)
        ).fetchall()
        conn.close()
        results = []
        for r in rows:
            try:
                if kw in json.loads(r['keywords'] or '[]'):
                    results.append(f"[{r['key']}] {r['content']}")
            except Exception:
                pass
        return results[:limit]
    except Exception as e:
        print(f'[engram] query_by_keyword failed: {e}', file=sys.stderr)
        return []


def journal(agent: str, action: str, outcome: str, context: str = '') -> bool:
    """Append a structured entry to the activity journal."""
    content = f'{agent}: {action} -> {outcome}'
    if context:
        content += f' (context: {context})'

    embedding = embed(content)
    try:
        conn = _db()
        cur = conn.execute(
            'INSERT INTO activity_journal (agent, action, outcome, context) VALUES (?,?,?,?)',
            (agent, action, outcome, context))
        rowid = cur.lastrowid
        if embedding:
            conn.execute('INSERT INTO journal_vec(rowid, embedding) VALUES (?,?)',
                         (rowid, sqlite_vec.serialize_float32(embedding)))
        conn.commit()
        conn.close()
        return True
    except Exception as e:
        print(f'[engram] journal failed: {e}', file=sys.stderr)
        return False


def get_conventions(topic: str = '') -> str:
    """Get relevant conventions/patterns. Returns markdown text."""
    if topic:
        results = query(topic, layer='patterns', top_k=5)
        if results:
            return '\n\n'.join(results)

    # Return all patterns
    try:
        conn = _db()
        rows = conn.execute('SELECT key, content FROM patterns ORDER BY created_at DESC LIMIT 20').fetchall()
        conn.close()
        if rows:
            return '\n\n'.join(f'## {r["key"]}\n{r["content"]}' for r in rows)
    except Exception:
        pass
    return '# Conventions\nNo conventions stored yet.'


def get_recent(hours_back: int = 24, agent_filter: str = '') -> List[dict]:
    """Recent journal entries for context loading."""
    try:
        conn = _db()
        query_sql = '''SELECT agent, action, outcome, context, created_at
                       FROM activity_journal
                       WHERE created_at > datetime('now', '-' || ? || ' hours')'''
        params = [str(hours_back)]
        if agent_filter:
            query_sql += ' AND agent = ?'
            params.append(agent_filter)
        query_sql += ' ORDER BY created_at DESC LIMIT 50'
        rows = conn.execute(query_sql, params).fetchall()
        conn.close()
        return [dict(r) for r in rows]
    except Exception as e:
        print(f'[engram] get_recent failed: {e}', file=sys.stderr)
        return []


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest='cmd')

    p_store = subparsers.add_parser('store')
    p_store.add_argument('--key', required=True)
    p_store.add_argument('--content', required=True)
    p_store.add_argument('--layer', default='kb', choices=['kb', 'patterns'])
    p_store.add_argument('--tags', default='')

    p_query = subparsers.add_parser('query')
    p_query.add_argument('--text', required=True)
    p_query.add_argument('--layer', default=None)
    p_query.add_argument('--top-k', type=int, default=5)

    p_journal = subparsers.add_parser('journal')
    p_journal.add_argument('--agent', required=True)
    p_journal.add_argument('--action', required=True)
    p_journal.add_argument('--outcome', required=True)
    p_journal.add_argument('--context', default='')

    p_conv = subparsers.add_parser('conventions')
    p_conv.add_argument('--topic', default='')

    p_recent = subparsers.add_parser('recent')
    p_recent.add_argument('--hours', type=int, default=24)
    p_recent.add_argument('--agent', default='')

    args = parser.parse_args()

    # Load env from axon .env
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip())

    # Refresh globals
    LLM_KEY = os.environ.get('LITELLM_MASTER_KEY', LLM_KEY)
    GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')

    if args.cmd == 'store':
        tags = [t.strip() for t in args.tags.split(',') if t.strip()]
        ok = store(args.key, args.content, args.layer, tags)
        print('stored' if ok else 'failed')
    elif args.cmd == 'query':
        results = query(args.text, args.layer, args.top_k)
        print(json.dumps(results, indent=2))
    elif args.cmd == 'journal':
        ok = journal(args.agent, args.action, args.outcome, args.context)
        print('logged' if ok else 'failed')
    elif args.cmd == 'conventions':
        print(get_conventions(args.topic))
    elif args.cmd == 'recent':
        entries = get_recent(args.hours, args.agent)
        print(json.dumps(entries, indent=2))
