"""
Reflexa — Engram memory system for ARCADE/Vector dev loops.
Option A: Python library with sqlite-vec local storage.

Usage:
    import reflexa
    reflexa.write('T1', {'iteration_id': '...', 'task': '...', 'gate_result': 'pass'})
    reflexa.flush()  # embed and store all queued entries
    reflexa.failures()  # surface prior write failures

CLI:
    python3 reflexa.py write T1 --payload '{"task":"..."}' --project "my-project"
    python3 reflexa.py flush
    python3 reflexa.py failures
    python3 reflexa.py query --text "how to handle auth"
"""

import os, json, time, logging, threading
from pathlib import Path
from datetime import datetime
from typing import Optional

log = logging.getLogger('reflexa')

# Config from environment (or reflexa.env)
DB_PATH = os.environ.get('REFLEXA_DB_PATH', os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db'))
EMBED_URL = os.environ.get('REFLEXA_EMBED_URL', 'http://YOUR_LITELLM_IP:4000/v1/embeddings')
EMBED_MODEL = os.environ.get('REFLEXA_EMBED_MODEL', 'text-embedding')
LLM_URL = os.environ.get('REFLEXA_LLM_URL', 'http://YOUR_LITELLM_IP:4000/v1/chat/completions')
LLM_MODEL = os.environ.get('REFLEXA_LLM_MODEL', 'qwen2.5:7b')
LLM_KEY = os.environ.get('REFLEXA_LLM_API_KEY', os.environ.get('LITELLM_MASTER_KEY', ''))
NOVELTY_THRESHOLD = float(os.environ.get('REFLEXA_NOVELTY_THRESHOLD', '0.6'))
TOP_K = int(os.environ.get('REFLEXA_TOP_K', '3'))
FAILURE_LOG = os.environ.get('REFLEXA_FAILURE_LOG', '/opt/lumina-fleet/engram/write_failures.jsonl')
AVAILABLE = os.environ.get('REFLEXA_AVAILABLE', '1') == '1'
MAX_RETRIES = int(os.environ.get('REFLEXA_MAX_RETRIES', '3'))

# Embedding dimension — must match what LiteLLM/text-embedding returns
EMBED_DIM = int(os.environ.get('REFLEXA_EMBED_DIM', '1024'))

# In-memory write queue (flushed at session end)
_queue = []
_queue_lock = threading.Lock()


# ── PUBLIC API ────────────────────────────────────────────────────────────────

def write(trigger_type: str, payload: dict, project: str = '', session_id: str = '') -> bool:
    """Queue a memory write. trigger_type: T1 (iteration), T2 (gate report), T3 (session close).
    Returns True if queued, False if Reflexa is disabled."""
    if not AVAILABLE:
        return False

    entry = {
        'trigger': trigger_type,
        'project': project,
        'session_id': session_id,
        'payload': payload,
        'queued_at': datetime.utcnow().isoformat() + 'Z',
    }
    with _queue_lock:
        _queue.append(entry)
    log.debug(f'Queued {trigger_type} entry ({len(_queue)} total)')
    return True


def flush() -> dict:
    """Embed and store all queued entries. Call at session end.
    Returns {stored: N, skipped: N, failed: N}."""
    if not AVAILABLE or not _queue:
        return {'stored': 0, 'skipped': 0, 'failed': 0}

    with _queue_lock:
        to_process = list(_queue)
        _queue.clear()

    stored = skipped = failed = 0

    for entry in to_process:
        for attempt in range(MAX_RETRIES):
            try:
                content = _entry_to_text(entry)
                novelty = _score_novelty(content)
                if novelty < NOVELTY_THRESHOLD and entry['trigger'] != 'T3':
                    skipped += 1
                    break
                _store_entry(entry, content, novelty)
                stored += 1
                break
            except Exception as e:
                if attempt == MAX_RETRIES - 1:
                    failed += 1
                    _log_failure(entry, str(e))
                else:
                    time.sleep(2 ** attempt)  # 1s, 2s, 4s backoff

    log.info(f'Flush complete: stored={stored} skipped={skipped} failed={failed}')
    return {'stored': stored, 'skipped': skipped, 'failed': failed}


def failures() -> list:
    """Surface prior write failures for review at session start."""
    if not Path(FAILURE_LOG).exists():
        return []
    failures_list = []
    try:
        with open(FAILURE_LOG) as f:
            for line in f:
                try:
                    failures_list.append(json.loads(line))
                except Exception:
                    pass
    except Exception:
        pass
    return failures_list[-50:]  # Last 50 failures


def query(topic: str) -> list:
    """Search memory for relevant prior patterns. Returns list of text excerpts."""
    if not AVAILABLE:
        return []
    try:
        import sqlite_vec
        import sqlite3
        if not Path(DB_PATH).exists():
            return []
        embedding = _embed(topic)
        if not embedding:
            return []
        conn = sqlite3.connect(DB_PATH)
        conn.enable_load_extension(True)
        sqlite_vec.load(conn)
        conn.enable_load_extension(False)
        try:
            # Similarity search via vec0 table
            rows = conn.execute(
                '''SELECT me.content, me.trigger_type, me.project, me.novelty_score,
                          distance
                   FROM memory_entries_vec mev
                   JOIN memory_entries me ON me.rowid = mev.rowid
                   ORDER BY vec_distance_L2(mev.embedding, ?) ASC
                   LIMIT ?''',
                (json.dumps(embedding), TOP_K)
            ).fetchall()
            results = [
                {'content': r[0], 'trigger': r[1], 'project': r[2],
                 'novelty': r[3], 'distance': r[4]}
                for r in rows
            ]
        except Exception as e:
            log.warning(f'Vector search failed (table may not exist yet): {e}')
            results = []
        conn.close()
        return results
    except Exception as e:
        log.warning(f'Query failed: {e}')
        return []


# ── INTERNALS ─────────────────────────────────────────────────────────────────

def _entry_to_text(entry: dict) -> str:
    p = entry.get('payload', {})
    if entry['trigger'] == 'T1':
        return (f"T1 Iteration {p.get('iteration_id','?')}: "
                f"task={p.get('task','?')}, gate={p.get('gate_result','?')}, "
                f"tokens={p.get('tokens','?')}")
    elif entry['trigger'] == 'T2':
        return (f"T2 Gate report: task={p.get('task_name','?')}, "
                f"result={p.get('gate_result','?')}, corrections={p.get('corrections','?')}")
    elif entry['trigger'] == 'T3':
        return (f"T3 Session {p.get('session_id', entry.get('session_id','?'))}: "
                f"project={entry.get('project','?')}, "
                f"iterations={p.get('total_iterations','?')}, "
                f"pass_rate={p.get('pass_rate','?')}")
    return json.dumps(p)


def _score_novelty(content: str) -> float:
    """Score 0.0-1.0 how novel/worth-storing this content is."""
    try:
        import urllib.request
        data = json.dumps({
            'model': LLM_MODEL,
            'messages': [{'role': 'user',
                'content': (
                    'Rate the novelty/learning value of this dev loop entry from 0.0 to 1.0. '
                    'Return only the number.\n\n' + content
                )}],
            'max_tokens': 10,
            'temperature': 0.1
        }).encode()
        req = urllib.request.Request(
            LLM_URL, data=data,
            headers={'Authorization': f'Bearer {LLM_KEY}', 'Content-Type': 'application/json'},
            method='POST'
        )
        with urllib.request.urlopen(req, timeout=15) as r:
            resp = json.load(r)['choices'][0]['message']['content'].strip()
            return float(resp.split()[0])
    except Exception:
        # Fallback: store T2 and T3 always, T1 half the time
        entry_type = content[:2] if content[:2] in ('T1', 'T2', 'T3') else 'T1'
        return 0.9 if entry_type in ('T2', 'T3') else 0.5


def _embed(text: str) -> Optional[list]:
    try:
        import urllib.request
        data = json.dumps({'model': EMBED_MODEL, 'input': text}).encode()
        req = urllib.request.Request(
            EMBED_URL, data=data,
            headers={'Authorization': f'Bearer {LLM_KEY}', 'Content-Type': 'application/json'},
            method='POST'
        )
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.load(r)['data'][0]['embedding']
    except Exception:
        return None


def _store_entry(entry: dict, content: str, novelty: float):
    """Store entry in sqlite-vec database with optional embedding."""
    import sqlite3, sqlite_vec, uuid as _uuid

    db_path = DB_PATH
    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)

    # Ensure tables exist
    conn.execute('''CREATE TABLE IF NOT EXISTS memory_entries (
        id TEXT PRIMARY KEY, trigger_type TEXT, project TEXT, content TEXT,
        raw_payload TEXT, created_at TEXT, session_id TEXT, novelty_score REAL
    )''')
    try:
        conn.execute(
            f'CREATE VIRTUAL TABLE IF NOT EXISTS memory_entries_vec '
            f'USING vec0(embedding float[{EMBED_DIM}])'
        )
    except Exception:
        pass  # May already exist with different definition

    entry_id = str(_uuid.uuid4())
    conn.execute(
        'INSERT INTO memory_entries VALUES (?,?,?,?,?,?,?,?)',
        (entry_id, entry['trigger'], entry.get('project', ''), content,
         json.dumps(entry['payload']), entry['queued_at'],
         entry.get('session_id', ''), novelty)
    )

    embedding = _embed(content)
    if embedding:
        try:
            # Get rowid of the inserted entry
            rowid = conn.execute(
                'SELECT rowid FROM memory_entries WHERE id=?', (entry_id,)
            ).fetchone()[0]
            conn.execute(
                'INSERT INTO memory_entries_vec(rowid, embedding) VALUES (?, ?)',
                (rowid, json.dumps(embedding))
            )
        except Exception as e:
            log.warning(f'Vector insert failed (non-fatal): {e}')

    conn.commit()
    conn.close()


def _log_failure(entry: dict, error: str):
    try:
        with open(FAILURE_LOG, 'a') as f:
            f.write(json.dumps({
                'ts': datetime.utcnow().isoformat(),
                'trigger': entry.get('trigger'),
                'error': error,
                'payload': entry.get('payload', {})
            }) + '\n')
    except Exception as log_err:
        log.error(f'Failed to write failure log: {log_err}')


# ── CLI ───────────────────────────────────────────────────────────────────────

if __name__ == '__main__':
    import argparse

    logging.basicConfig(level=logging.INFO, format='%(levelname)s %(message)s')

    parser = argparse.ArgumentParser(description='Reflexa memory system CLI')
    sub = parser.add_subparsers(dest='cmd', required=True)

    # write
    p_write = sub.add_parser('write', help='Queue and immediately flush a single entry')
    p_write.add_argument('trigger', choices=['T1', 'T2', 'T3'], help='Trigger type')
    p_write.add_argument('--payload', required=True, help='JSON payload string')
    p_write.add_argument('--project', default='', help='Project name')
    p_write.add_argument('--session', default='', help='Session ID')

    # flush
    sub.add_parser('flush', help='Flush all queued entries to storage')

    # failures
    sub.add_parser('failures', help='List prior write failures')

    # query
    p_query = sub.add_parser('query', help='Semantic search memory')
    p_query.add_argument('--text', required=True, help='Search text')

    args = parser.parse_args()

    if args.cmd == 'write':
        try:
            payload = json.loads(args.payload)
        except json.JSONDecodeError as e:
            print(f'ERROR: invalid JSON payload: {e}')
            raise SystemExit(1)
        ok = write(args.trigger, payload, project=args.project, session_id=args.session)
        if ok:
            result = flush()
            print(json.dumps({'queued': True, 'flush': result}))
        else:
            print(json.dumps({'queued': False, 'reason': 'REFLEXA_AVAILABLE=0'}))

    elif args.cmd == 'flush':
        result = flush()
        print(json.dumps(result))

    elif args.cmd == 'failures':
        result = failures()
        if result:
            for f_entry in result:
                print(json.dumps(f_entry))
        else:
            print(json.dumps({'failures': 0}))

    elif args.cmd == 'query':
        results = query(args.text)
        print(json.dumps(results, indent=2))
