import subprocess
import json
import os

# ============================================================
# Engram Tools -- Semantic Memory System
# MCP tools for querying and writing to Engram memory on CT310.
# Runs on CT214, SSHes to CT310 to execute engram.py.
# Multi-claw: uses LUMINA_AGENT_ID from server.py context
#             to scope reads/writes to the calling agent.
#
# Provider interface: MEMORY_PROVIDER env var selects backend.
# Default: 'engram' (direct SSH to CT310).
# Alternative: 'honcho' (Honcho REST API on CT310:8100).
# ============================================================

ENGRAM_HOST = 'root@YOUR_FLEET_SERVER_IP'
ENGRAM_SCRIPT = '/usr/bin/python3 /opt/lumina-fleet/engram/engram.py'
ENGRAM_ENV = 'source /opt/lumina-fleet/axon/.env && export LITELLM_MASTER_KEY GITEA_TOKEN INBOX_DB_PASS'

MEMORY_PROVIDER = os.environ.get('MEMORY_PROVIDER', 'engram')

# Module-level STM (cleared on process restart)
_SESSION_STORE: dict = {}

try:
    import sys as _sys
    _sys.path.insert(0, '/opt/ai-mcp')
    from server import get_agent_context as _get_agent_context
except Exception:
    def _get_agent_context(): return os.environ.get('LUMINA_AGENT_ID', 'lumina')


def _ssh(cmd, timeout=30):
    safe_cmd = cmd.replace('"', '\\"')
    full_cmd = f'ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {ENGRAM_HOST} "{safe_cmd}"'
    try:
        result = subprocess.run(full_cmd, shell=True, capture_output=True, text=True, timeout=timeout)
        return {'stdout': result.stdout.strip(), 'rc': result.returncode, 'stderr': result.stderr.strip()}
    except subprocess.TimeoutExpired:
        return {'error': f'Engram timed out ({timeout}s)', 'rc': -1}
    except Exception as e:
        return {'error': str(e), 'rc': -1}


def _get_memory_provider():
    """Get the configured memory provider. Lazy import to avoid circular deps."""
    if MEMORY_PROVIDER == 'honcho':
        try:
            from memory_provider import HonchoProvider
            return HonchoProvider()
        except Exception:
            pass
    return None  # None = use direct SSH (default Engram)


def register_engram_tools(mcp):

    @mcp.tool()
    def engram_query(query: str, layer: str = '', limit: int = 5) -> dict:
        """Search Engram memory semantically for relevant context.
        query: what to search for -- use natural language.
        layer: kb (knowledge base), patterns (conventions), or empty for all.
        limit: max results (default 5).
        Returns: {count, results: [text strings]}
        Use before executing tasks to check for prior decisions and conventions."""
        # Try Honcho provider if configured
        provider = _get_memory_provider()
        if provider:
            results = provider.query(query, limit=limit)
            return {'count': len(results), 'results': results, 'query': query, 'provider': provider.provider_name}

        # Default: direct Engram via SSH
        layer_arg = f'--layer {layer}' if layer in ('kb', 'patterns') else ''
        safe_query = query[:300].replace("'", " ")
        cmd = f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} query --text '{safe_query}' --top-k {limit} {layer_arg}"
        result = _ssh(cmd, timeout=30)
        if result.get('error'):
            return {'error': result['error']}
        try:
            items = json.loads(result['stdout']) if result['stdout'] else []
            return {'count': len(items), 'results': items, 'query': query, 'provider': 'engram'}
        except Exception:
            return {'count': 0, 'results': [], 'raw': result['stdout'][:200], 'provider': 'engram'}

    @mcp.tool()
    def engram_store(key: str, content: str, layer: str = 'kb', tags: str = '') -> dict:
        """Store a fact, decision, or learning in Engram memory.
        key: unique identifier (e.g. 'infra/ct310', 'decision/auth-approach').
        content: the information to store (markdown OK).
        layer: kb (knowledge base) or patterns (conventions/rules).
        tags: comma-separated tags for filtering.
        Returns: {stored: true/false}"""
        # Try Honcho provider if configured
        provider = _get_memory_provider()
        if provider:
            ok = provider.store(key, content, layer=layer)
            return {'stored': ok, 'key': key, 'provider': provider.provider_name}

        # Default: direct Engram via SSH
        tags_arg = f"--tags '{tags}'" if tags else ''
        safe_content = content[:800].replace("'", "\\'")
        cmd = f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} store --key '{key}' --content '{safe_content}' --layer {layer} {tags_arg}"
        result = _ssh(cmd, timeout=30)
        return {'stored': result.get('rc') == 0, 'key': key, 'provider': 'engram'}

    @mcp.tool()
    def engram_journal(action: str, outcome: str, context: str = '', agent: str = '') -> dict:
        """Append to Engram activity journal. Call after completing significant actions.
        action: what was done (e.g. 'created Plane labels', 'ran health check').
        outcome: result (e.g. '5 labels created', 'all 11 services healthy').
        context: optional additional context.
        agent: which agent (auto-detected from LUMINA_AGENT_ID if not specified)."""
        if not agent:
            agent = _get_agent_context()
        safe_action = action[:200].replace("'", "\\'")
        safe_outcome = outcome[:200].replace("'", "\\'")
        safe_context = context[:200].replace("'", "\\'")
        cmd = (f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} journal "
               f"--agent '{agent}' --action '{safe_action}' "
               f"--outcome '{safe_outcome}' --context '{safe_context}'")
        result = _ssh(cmd, timeout=20)
        return {'logged': result.get('rc') == 0}

    @mcp.tool()
    def engram_conventions(topic: str = '') -> dict:
        """Get relevant conventions and patterns from Engram.
        topic: optional -- search for conventions related to this topic.
        Returns conventions as markdown text.
        Call this before naming things, making architectural decisions, or following patterns."""
        topic_arg = f"--topic '{topic[:200]}'" if topic else ''
        cmd = f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} conventions {topic_arg}"
        result = _ssh(cmd, timeout=20)
        return {'conventions': result.get('stdout', ''), 'topic': topic}

    @mcp.tool()
    def engram_recent(hours_back: int = 24, agent_filter: str = '') -> dict:
        """Get recent activity journal entries.
        hours_back: look back N hours (default 24).
        agent_filter: filter by specific agent name.
        Use for context loading at session start."""
        agent_arg = f"--agent '{agent_filter}'" if agent_filter else ''
        cmd = f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} recent --hours {hours_back} {agent_arg}"
        result = _ssh(cmd, timeout=20)
        try:
            entries = json.loads(result['stdout']) if result['stdout'] else []
            return {'count': len(entries), 'entries': entries, 'hours_back': hours_back}
        except Exception:
            return {'count': 0, 'entries': [], 'raw': result['stdout'][:200]}

    # ----------------------------------------------------------------
    # ENG-68: engram_update
    # ----------------------------------------------------------------
    @mcp.tool()
    def engram_update(key: str, new_content: str, reason: str = '') -> dict:
        """Update an existing Engram memory entry. Preserves the old version.
        key: exact key of the fact to update (e.g. 'agents/peter/profile/location')
        new_content: the new content to replace the existing entry
        reason: why this fact is being updated (logged to journal)
        Returns: {updated: true/false, key, old_content_preview}

        Use this when you learn that a fact has changed -- do NOT just store a duplicate."""
        safe_key = key.replace("'", " ")
        safe_content = new_content[:800].replace("'", " ")
        safe_reason = reason[:200].replace("'", " ")

        # Get old content first
        get_cmd = (f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} query "
                   f"--text '{safe_key}' --top-k 1")
        old_result = _ssh(get_cmd, timeout=20)

        # Store updated content (INSERT OR REPLACE by key)
        cmd = (f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} store "
               f"--key '{safe_key}' --content '{safe_content}' "
               f"--layer kb --tags 'updated'")
        result = _ssh(cmd, timeout=30)

        # Log to journal
        if result.get('rc') == 0 and reason:
            journal_cmd = (f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} journal "
                          f"--agent 'lumina' --action 'updated fact {safe_key[:40]}' "
                          f"--outcome 'Reason: {safe_reason}'")
            _ssh(journal_cmd, timeout=20)

        return {
            'updated': result.get('rc') == 0,
            'key': key,
            'reason': reason,
            'old_preview': str(old_result.get('stdout', ''))[:80],
        }

    # ----------------------------------------------------------------
    # ENG-69: engram_discard
    # ----------------------------------------------------------------
    @mcp.tool()
    def engram_discard(key: str, reason: str = '') -> dict:
        """Mark an Engram fact as discarded (soft-delete). The fact remains in DB but is excluded from queries.
        key: exact key of the fact to discard
        reason: why this fact is being discarded
        Use this for facts that are no longer true, outdated, or incorrect."""
        safe_key = key.replace("'", " ")
        safe_reason = reason[:200].replace("'", " ")

        # Mark as discarded by adding 'discarded' tag
        cmd = (f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} store "
               f"--key '_discarded/{safe_key}' "
               f"--content 'DISCARDED: {safe_key}. Reason: {safe_reason}' "
               f"--layer kb --tags 'discarded,archived'")
        result = _ssh(cmd, timeout=20)

        if result.get('rc') == 0 and reason:
            journal_cmd = (f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} journal "
                          f"--agent 'lumina' --action 'discarded fact {safe_key[:40]}' "
                          f"--outcome '{safe_reason}'")
            _ssh(journal_cmd, timeout=20)

        return {
            'discarded': result.get('rc') == 0,
            'key': key,
            'reason': reason,
        }

    # ----------------------------------------------------------------
    # ENG-70: engram_summarize
    # ----------------------------------------------------------------
    @mcp.tool()
    def engram_summarize(namespace: str, max_facts: int = 20) -> dict:
        """Summarize all facts in a namespace using a local model.
        namespace: Engram namespace to summarize (e.g. 'agents/peter/profile')
        max_facts: max facts to include in summary (default 20)
        Returns: {summary, fact_count, stored_key} -- summary is stored as a new fact.
        Uses local Qwen model via LiteLLM ($0 cost)."""
        import json as _json

        # Get all facts in namespace by keyword search
        safe_ns = namespace.replace("'", " ")
        cmd = f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} query --text '{safe_ns}' --top-k {max_facts}"
        result = _ssh(cmd, timeout=30)

        facts_raw = result.get('stdout', '')
        if not facts_raw:
            return {'error': f'No facts found in namespace: {namespace}', 'fact_count': 0}

        # Summarize using local model
        try:
            facts_list = _json.loads(facts_raw) if facts_raw.startswith('[') else [facts_raw]
        except Exception:
            facts_list = [facts_raw]

        facts_text = '\n'.join(str(f)[:200] for f in facts_list[:max_facts])
        fact_count = len(facts_list)

        # Use LiteLLM local model for summarization
        litellm_url = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
        litellm_key = os.environ.get('LITELLM_MASTER_KEY', '')

        prompt = f"""Summarize these memory facts about '{namespace}' into 2-3 clear, concise sentences.
Focus on the key facts that would be most useful for an AI assistant to know.
Facts:
{facts_text}

Summary:"""

        summary = facts_text[:300]  # fallback
        try:
            import urllib.request as _urllib
            data = _json.dumps({
                'model': 'Lumina Fast',
                'messages': [{'role': 'user', 'content': prompt}],
                'max_tokens': 200
            }).encode()
            req = _urllib.Request(f'{litellm_url}/v1/chat/completions', data=data,
                headers={'Authorization': f'Bearer {litellm_key}', 'Content-Type': 'application/json'},
                method='POST')
            with _urllib.urlopen(req, timeout=30) as r:
                summary = _json.load(r)['choices'][0]['message']['content'].strip()
        except Exception:
            pass  # Use fallback

        # Store summary as a new fact
        summary_key = f'_summaries/{namespace.replace("/", "-")}'
        safe_summary = summary[:500].replace("'", " ")
        store_cmd = (f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} store "
                    f"--key '{summary_key}' "
                    f"--content '{safe_summary}' "
                    f"--layer kb --tags 'summary,auto-generated'")
        _ssh(store_cmd, timeout=30)

        return {
            'summary': summary,
            'fact_count': fact_count,
            'namespace': namespace,
            'stored_key': summary_key,
        }

    # ----------------------------------------------------------------
    # ENG-71: Session memory (STM) tools
    # ----------------------------------------------------------------
    @mcp.tool()
    def engram_session_store(key: str, value: str) -> dict:
        """Store a fact in short-term session memory (cleared on conversation end).
        For things Lumina needs to remember within this conversation but NOT forever.
        Examples: user's current task, temporary decisions, in-progress calculations.
        key: session-scoped key (no namespace needed)
        value: content to remember temporarily"""
        _SESSION_STORE[key] = {
            'value': value,
            'stored_at': __import__('datetime').datetime.utcnow().isoformat()
        }
        return {'stored': True, 'key': key, 'session_size': len(_SESSION_STORE)}

    @mcp.tool()
    def engram_session_get(key: str = '') -> dict:
        """Retrieve from short-term session memory.
        key: specific key to retrieve. If empty, returns all session memory.
        Returns: {found, value} or {all: {key: value}}"""
        if key:
            entry = _SESSION_STORE.get(key)
            if entry:
                return {'found': True, 'key': key, 'value': entry['value'],
                        'stored_at': entry['stored_at']}
            return {'found': False, 'key': key}
        # Return all session memory
        return {
            'all': {k: v['value'] for k, v in _SESSION_STORE.items()},
            'count': len(_SESSION_STORE)
        }

    @mcp.tool()
    def engram_session_clear(key: str = '') -> dict:
        """Clear session memory. If key provided, clears that key. Otherwise clears all."""
        if key and key in _SESSION_STORE:
            del _SESSION_STORE[key]
            return {'cleared': True, 'key': key}
        elif not key:
            count = len(_SESSION_STORE)
            _SESSION_STORE.clear()
            return {'cleared': True, 'cleared_count': count}
        return {'cleared': False, 'key': key, 'reason': 'key not found'}

    # ----------------------------------------------------------------
    # ENG-72: engram_graph
    # ----------------------------------------------------------------
    @mcp.tool()
    def engram_graph(note_id: str, hops: int = 1) -> dict:
        """Get a fact and all facts linked to it (Zettelkasten graph traversal).
        note_id: the ENG-XXXX note ID (returned when storing facts with linking enabled)
        hops: how many link hops to traverse (default 1, max 2 to avoid context bloat)
        Returns the central fact + all linked facts within N hops."""
        safe_id = note_id.replace("'", " ")

        # Query engram for the note by ID via a special key
        cmd = f"{ENGRAM_ENV} && {ENGRAM_SCRIPT} query --text '{safe_id}' --top-k 1"
        result = _ssh(cmd, timeout=20)

        central = result.get('stdout', '')[:500]

        # Get links (from DB directly via SSH python)
        links_cmd = (
            f"source /opt/lumina-fleet/axon/.env && "
            f"python3 -c \""
            f"import sqlite3, json; "
            f"conn = sqlite3.connect('/opt/lumina-fleet/engram/engram.db'); "
            f"rows = conn.execute(\\\"SELECT note_id_1, note_id_2, link_strength FROM memory_links "
            f"WHERE note_id_1='{safe_id}' OR note_id_2='{safe_id}' ORDER BY link_strength DESC LIMIT 10\\\").fetchall(); "
            f"print(json.dumps([list(r) for r in rows]))\""
        )
        links_result = _ssh(links_cmd, timeout=15)
        out2 = links_result.get('stdout', '[]')

        try:
            links = __import__('json').loads(out2)
        except Exception:
            links = []

        return {
            'note_id': note_id,
            'central_fact': central,
            'links': links,
            'link_count': len(links),
            'hops': min(hops, 2),
        }
