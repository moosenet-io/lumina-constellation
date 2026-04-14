#!/usr/bin/env python3
"""
Mr. Wizard — Deep Reasoning Agent
Convenes the Obsidian Circle council for complex problems.
Runs on CT310 as part of lumina-fleet.
"""

import os, sys, json, uuid, time, argparse
import urllib.request, psycopg2
from datetime import datetime
from pathlib import Path

LITELLM_URL = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
COUNCIL_DIR = Path(os.path.dirname(__file__)) / 'council'
SESSIONS_DIR = Path(os.path.dirname(__file__)) / 'sessions'
GITEA_URL = os.environ.get('GITEA_URL', 'http://YOUR_GITEA_IP:3000')
GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')

INBOX_DB_HOST = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
INBOX_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', '')

# Council member config
COUNCIL_MEMBERS = [
    ('architect-arcane', 'architect-arcane.md'),
    ('skeptic-seer', 'skeptic-seer.md'),
    ('keeper-of-operations', 'keeper-of-operations.md'),
    ('wandering-fool', 'wandering-fool.md'),
]

def llm_call(messages, model='qwen2.5:7b', max_tokens=800):
    data = json.dumps({'model': model, 'messages': messages, 'max_tokens': max_tokens, 'temperature': 0.7}).encode()
    req = urllib.request.Request(
        f'{LITELLM_URL}/v1/chat/completions', data=data,
        headers={'Authorization': f'Bearer {LITELLM_KEY}', 'Content-Type': 'application/json'}, method='POST')
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.load(r)['choices'][0]['message']['content'].strip()

def load_persona(filename):
    path = COUNCIL_DIR / filename
    if path.exists():
        return path.read_text()
    return f'You are a council member. Provide your perspective on the problem.'

def run_solo_consultation(query, context, model):
    """Auto-tier: single Sonnet pass, no council."""
    system = """You are Mr. Wizard, a deep reasoning assistant for MooseNet infrastructure.
Provide precise, actionable analysis. Structure your response as:
1. Problem restatement (1-2 sentences)
2. Analysis (key considerations, trade-offs)
3. Recommendation (specific, actionable)
4. Confidence level and key caveats"""

    user_msg = f'Question: {query}'
    if context:
        user_msg += f'\n\nContext: {context}'

    return llm_call(
        [{'role': 'system', 'content': system}, {'role': 'user', 'content': user_msg}],
        model=model, max_tokens=1000
    )

def run_council_session(query, context, model):
    """Convene the Obsidian Circle — 4 perspectives + synthesis."""
    perspectives = []

    for name, filename in COUNCIL_MEMBERS:
        persona = load_persona(filename)
        messages = [
            {'role': 'system', 'content': persona},
            {'role': 'user', 'content': f'The council has been convened for this question:\n\n{query}' + (f'\n\nContext: {context}' if context else '') + f'\n\nProvide your perspective as {name.replace("-", " ").title()}.'}
        ]
        perspective = llm_call(messages, model=model, max_tokens=600)
        perspectives.append({'member': name, 'perspective': perspective})
        print(f'[wizard] {name} perspective complete')

    # Synthesis
    council_text = '\n\n'.join(f'## {p["member"].replace("-"," ").title()}\n{p["perspective"]}' for p in perspectives)
    synthesis = llm_call([
        {'role': 'system', 'content': 'You are Mr. Wizard. You have convened the Obsidian Circle and received four perspectives. Synthesize them into a unified recommendation.'},
        {'role': 'user', 'content': f'Question: {query}\n\n{council_text}\n\nSynthesize these perspectives into:\n1. Unified recommendation\n2. Key insights from the council\n3. Noted dissents or unresolved tensions\n4. Confidence and caveats'}
    ], model=model, max_tokens=1200)

    return synthesis, perspectives

def publish_to_engram(session_id, query, result, perspectives=None):
    """Push consultation result to lumina-engram."""
    date_str = datetime.now().strftime('%Y-%m-%d')
    slug = query[:30].lower().replace(' ', '-').replace('?', '').replace('/', '-')
    filename = f'wizard/{date_str}-{session_id[:8]}-{slug}.md'

    content = f'# {query}\n\n*{date_str} | session: {session_id[:8]}*\n\n## Analysis\n\n{result}'
    if perspectives:
        content += '\n\n## Council Perspectives\n\n' + '\n\n'.join(f'### {p["member"].replace("-"," ").title()}\n{p["perspective"]}' for p in perspectives)

    import base64
    encoded = base64.b64encode(content.encode()).decode()

    try:
        req = urllib.request.Request(
            f'{GITEA_URL}/api/v1/repos/moosenet/lumina-engram/contents/{filename}',
            data=json.dumps({'message': f'wizard: {query[:50]}', 'content': encoded}).encode(),
            headers={'Authorization': f'token {GITEA_TOKEN}', 'Content-Type': 'application/json'},
            method='POST')
        with urllib.request.urlopen(req, timeout=15) as r:
            json.load(r)
        return filename
    except Exception as e:
        print(f'[wizard] Engram push failed: {e}')
        return None

def nexus_send(msg_type, payload, priority='normal', correlation_id=''):
    """Send result back to Lumina via Nexus."""
    try:
        conn = psycopg2.connect(host=INBOX_DB_HOST, dbname='lumina_inbox',
            user=INBOX_DB_USER, password=INBOX_DB_PASS, connect_timeout=5)
        cur = conn.cursor()
        ttl = {'critical': 72, 'urgent': 48, 'normal': 24, 'low': 12}.get(priority, 24)
        cur.execute(
            "INSERT INTO inbox_messages (from_agent,to_agent,message_type,priority,payload,correlation_id,expires_at) VALUES ('wizard','lumina',%s,%s,%s,%s,now()+(%s||' hours')::interval) RETURNING id",
            (msg_type, priority, json.dumps(payload), correlation_id or None, str(ttl)))
        mid = str(cur.fetchone()[0])
        conn.commit(); conn.close()
        return mid
    except Exception as e:
        print(f'[wizard] nexus_send failed: {e}')
        return None

def consult(query, tier='auto', council=False, context='', correlation_id=''):
    session_id = str(uuid.uuid4())
    SESSIONS_DIR.mkdir(exist_ok=True)

    session = {
        'id': session_id, 'query': query, 'tier': tier, 'council_used': council or (tier == 'gated'),
        'status': 'running', 'created_at': datetime.utcnow().isoformat() + 'Z',
        'model': 'claude-sonnet-4-6' if tier in ('auto', 'gated') else 'qwen2.5:7b',
        'perspectives': []
    }

    # Save session state
    session_file = SESSIONS_DIR / f'{session_id}.json'
    session_file.write_text(json.dumps(session, indent=2))

    print(f'[wizard] Session {session_id[:8]} | tier={tier} | council={council}')

    model = 'claude-sonnet-4-6' if tier in ('auto', 'gated') else 'qwen2.5:7b'

    try:
        if council or tier == 'gated':
            result, perspectives = run_council_session(query, context, model)
            session['perspectives'] = perspectives
        else:
            result = run_solo_consultation(query, context, model)
            perspectives = None

        session['status'] = 'complete'
        session['result'] = result
        session['completed_at'] = datetime.utcnow().isoformat() + 'Z'
        session_file.write_text(json.dumps(session, indent=2))

        # Publish to Engram
        engram_path = publish_to_engram(session_id, query, result, session.get('perspectives'))
        session['engram_path'] = engram_path
        session_file.write_text(json.dumps(session, indent=2))

        # Send result to Lumina via Nexus
        nexus_mid = nexus_send('result', {
            'session_id': session_id,
            'query': query,
            'recommendation_preview': result[:300],
            'engram_path': engram_path,
            'council_used': council,
            'tier': tier,
        }, correlation_id=correlation_id)

        print(f'[wizard] Complete. Nexus result: {nexus_mid[:8] if nexus_mid else "failed"}')
        return {'session_id': session_id, 'status': 'complete', 'result': result, 'engram_path': engram_path}

    except Exception as e:
        session['status'] = 'failed'
        session['error'] = str(e)
        session_file.write_text(json.dumps(session, indent=2))
        nexus_send('escalation', {'session_id': session_id, 'error': str(e), 'query': query}, priority='urgent', correlation_id=correlation_id)
        raise

# ---------------------------------------------------------------------------
# OBC v2 — Async parallel deliberation (OBC-30)
# ---------------------------------------------------------------------------

import asyncio
import yaml


def _load_council_config():
    """Load council_config.yaml from the wizard directory."""
    config_path = Path(os.path.dirname(__file__)) / 'council_config.yaml'
    with open(config_path) as f:
        return yaml.safe_load(f)


def _http_post(url, headers, payload, timeout=45):
    """Synchronous HTTP POST helper, returns parsed JSON."""
    data = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method='POST')
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)


def _call_openrouter(model, messages, temperature, max_tokens):
    """Call OpenRouter chat completions API."""
    api_key = os.environ.get('OPENROUTER_API_KEY', '')
    if not api_key:
        raise ValueError('OPENROUTER_API_KEY not set')
    headers = {
        'Authorization': f'Bearer {api_key}',
        'Content-Type': 'application/json',
        'HTTP-Referer': 'https://moosenet.online',
        'X-Title': 'Obsidian Circle v2',
    }
    payload = {
        'model': model,
        'messages': messages,
        'temperature': temperature,
        'max_tokens': max_tokens,
    }
    resp = _http_post('https://openrouter.ai/api/v1/chat/completions', headers, payload)
    text = resp['choices'][0]['message']['content'].strip()
    usage = resp.get('usage', {})
    cost = usage.get('total_cost', None)
    model_used = resp.get('model', model)
    return text, cost, model_used


def _call_ollama(endpoint, model, prompt, temperature, max_tokens):
    """Call Ollama generate API."""
    headers = {'Content-Type': 'application/json'}
    payload = {
        'model': model,
        'prompt': prompt,
        'stream': False,
        'options': {'temperature': temperature, 'num_predict': max_tokens},
    }
    resp = _http_post(f'{endpoint}/api/generate', headers, payload)
    text = resp.get('response', '').strip()
    return text, 0.0, model  # Ollama is local — zero cost


def _call_litellm(model, messages, temperature, max_tokens):
    """Call LiteLLM proxy."""
    headers = {
        'Authorization': f'Bearer {LITELLM_KEY}',
        'Content-Type': 'application/json',
    }
    payload = {
        'model': model,
        'messages': messages,
        'temperature': temperature,
        'max_tokens': max_tokens,
    }
    resp = _http_post(f'{LITELLM_URL}/v1/chat/completions', headers, payload)
    text = resp['choices'][0]['message']['content'].strip()
    usage = resp.get('usage', {})
    cost = usage.get('total_cost', None)
    model_used = resp.get('model', model)
    return text, cost, model_used


def _messages_for_seat(seat_name, persona_text, query, context):
    """Build the chat messages list for a seat."""
    user_content = (
        f'The Obsidian Circle has been convened for this question:\n\n{query}'
    )
    if context:
        user_content += f'\n\nContext: {context}'
    user_content += f'\n\nProvide your perspective as {seat_name.replace("-", " ").title()}.'
    return [
        {'role': 'system', 'content': persona_text},
        {'role': 'user', 'content': user_content},
    ]


def call_seat_sync(seat_name, seat_config, query, context):
    """
    Synchronous call for a single council seat, trying routes in priority order.
    Returns (seat_name, response_text, cost_estimate, model_used).
    """
    persona_file = seat_config.get('persona_file', '')
    persona_text = load_persona(Path(persona_file).name) if persona_file else (
        f'You are {seat_name.replace("-", " ").title()}, a council member of the Obsidian Circle.'
    )

    temperature = seat_config.get('temperature', 0.5)
    max_tokens = seat_config.get('max_tokens', 600)
    messages = _messages_for_seat(seat_name, persona_text, query, context)
    # For Ollama we need a plain prompt string
    prompt_str = persona_text + '\n\n' + messages[-1]['content']

    routes = seat_config.get('route_priority', [])
    last_error = None

    for route in routes:
        route_type = route.get('type', 'litellm')
        route_model = route.get('model', seat_config.get('model', 'Lumina'))
        try:
            if route_type == 'openrouter':
                text, cost, used = _call_openrouter(route_model, messages, temperature, max_tokens)
            elif route_type == 'ollama':
                endpoint = route.get('endpoint', 'http://YOUR_GPU_HOST_IP:11434')
                text, cost, used = _call_ollama(endpoint, route_model, prompt_str, temperature, max_tokens)
            elif route_type == 'litellm':
                text, cost, used = _call_litellm(route_model, messages, temperature, max_tokens)
            else:
                raise ValueError(f'Unknown route type: {route_type}')

            print(f'[wizard/obc-v2] {seat_name} via {route_type}/{route_model} — ok')
            return seat_name, text, cost, used

        except Exception as e:
            print(f'[wizard/obc-v2] {seat_name} via {route_type}/{route_model} failed: {e}')
            last_error = e
            continue

    # All routes exhausted
    raise RuntimeError(f'All routes failed for {seat_name}. Last error: {last_error}')


async def call_seat_async(seat_name, seat_config, query, context):
    """
    Async wrapper for call_seat_sync — runs in a thread pool so the
    synchronous HTTP calls don't block the event loop.
    Returns (seat_name, response_text, cost_estimate, model_used).
    """
    return await asyncio.to_thread(call_seat_sync, seat_name, seat_config, query, context)


async def _council_deliberate_async(config, query, context):
    """Run all 4 seats in parallel, then synthesize."""
    seats = config.get('seats', {})
    tasks = [
        call_seat_async(seat_name, seat_cfg, query, context)
        for seat_name, seat_cfg in seats.items()
    ]
    results = await asyncio.gather(*tasks, return_exceptions=True)

    responses = {}
    costs = {}
    models_used = {}
    errors = {}

    for res in results:
        if isinstance(res, Exception):
            print(f'[wizard/obc-v2] A seat raised an exception: {res}')
            continue
        seat_name, text, cost, model_used = res
        responses[seat_name] = text
        costs[seat_name] = cost
        models_used[seat_name] = model_used

    # Synthesis via LiteLLM
    synth_cfg = config.get('synthesis', {})
    synth_model = synth_cfg.get('model', 'claude-sonnet-4-6')
    synth_max_tokens = synth_cfg.get('max_tokens', 1000)
    prompt_template = synth_cfg.get('prompt_template', '')

    def _safe(key):
        return responses.get(key, '[no response — seat failed]')

    if prompt_template:
        synth_prompt = prompt_template.format(
            query=query,
            architect_arcane=_safe('architect-arcane'),
            skeptic_seer=_safe('skeptic-seer'),
            keeper_of_operations=_safe('keeper-of-operations'),
            wandering_fool=_safe('wandering-fool'),
        )
    else:
        council_text = '\n\n'.join(
            f'## {k.replace("-", " ").title()}\n{v}' for k, v in responses.items()
        )
        synth_prompt = (
            f'Question: {query}\n\n{council_text}\n\n'
            'Synthesize into JSON: {recommendation, confidence, disagreements, summary}'
        )

    synth_messages = [
        {'role': 'system', 'content': 'You are Lumina synthesizing the Obsidian Circle deliberation.'},
        {'role': 'user', 'content': synth_prompt},
    ]

    try:
        synth_text, synth_cost, synth_model_used = await asyncio.to_thread(
            _call_litellm, synth_model, synth_messages, 0.3, synth_max_tokens
        )
        try:
            synthesis_data = json.loads(synth_text)
        except json.JSONDecodeError:
            synthesis_data = {'raw': synth_text}
    except Exception as e:
        print(f'[wizard/obc-v2] Synthesis failed: {e}')
        synth_cost = None
        synth_model_used = synth_model
        synthesis_data = {'error': str(e), 'raw': ''}

    return {
        'responses': responses,
        'models_used': models_used,
        'costs': costs,
        'synthesis': synthesis_data,
        'errors': errors,
    }


def _get_cortex_context(files):
    """Run Cortex blast-radius analysis on a list of file paths and return a summary string."""
    import subprocess
    import json as j
    files_str = ' '.join(files)
    cmd = f'python3 /opt/lumina-fleet/cortex/cortex.py review lumina-fleet {files_str}'
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=20)
    if r.returncode == 0:
        data = j.loads(r.stdout)
        return (
            f"Risk score: {data.get('risk_score', 0)}/10. "
            f"Blast radius: {data.get('blast_radius', [])}. "
            f"Signals: {data.get('risk_signals', [])}"
        )
    return 'Cortex unavailable'


def _journal_council_session(query, result):
    """Write a compact journal entry for the council session to Engram. Non-blocking."""
    import subprocess
    confidence = (
        result.get('synthesis', {}).get('confidence', 0)
        if isinstance(result.get('synthesis'), dict)
        else 0
    )
    summary = (
        result.get('synthesis', {}).get('summary', str(result.get('synthesis', ''))[:100])
        if isinstance(result.get('synthesis'), dict)
        else str(result.get('synthesis', ''))[:100]
    )
    total_cost = result.get('total_cost', 0)

    # Load env for Engram access
    from pathlib import Path as _Path
    env_file = _Path('/opt/lumina-fleet/axon/.env')
    import os as _os
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                v = v.strip().strip('"').strip("'")
                _os.environ.setdefault(k.strip(), v)

    key = f'wizard/council/{int(__import__("time").time())}'
    content = (
        f'Query: {query[:100]} | '
        f'Confidence: {confidence}/10 | '
        f'Cost: ${total_cost:.3f} | '
        f'Summary: {summary[:200]}'
    )
    cmd = (
        f'python3 /opt/lumina-fleet/engram/engram.py store '
        f'--key "{key}" --content "{content[:400]}" --layer journal 2>/dev/null'
    )
    subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=10)


def council_deliberate_parallel(query, context='', cortex_files=None):
    """
    OBC v2 main entry point — genuine multi-model parallel deliberation.

    Loads council_config.yaml, fires all 4 seats concurrently via
    asyncio.gather(), collects responses, runs synthesis via LiteLLM,
    and returns a full deliberation dict.

    Falls back to sequential execution if asyncio fails.

    Args:
        query (str): The question or problem to deliberate on.
        context (str): Optional additional context.
        cortex_files (list[str]|None): File paths to pass through Cortex code analysis
            before deliberation. Results are prepended to context for all seats.

    Returns:
        dict with keys:
            session_id, query, responses (per-seat), models_used, costs,
            synthesis (parsed JSON or raw), total_cost, status
    """
    # OBC-37: inject Cortex code analysis into context before seats run
    if cortex_files:
        print(f'[wizard/obc-v2] Running Cortex on {cortex_files}')
        cortex_context = _get_cortex_context(cortex_files)
        context = f'Code analysis from Cortex:\n{cortex_context}\n\n{context}'

    session_id = str(uuid.uuid4())
    print(f'[wizard/obc-v2] Starting parallel deliberation {session_id[:8]}')

    try:
        config = _load_council_config()
    except Exception as e:
        raise RuntimeError(f'Failed to load council_config.yaml: {e}')

    deliberation = None

    # Try async parallel path first
    try:
        try:
            loop = asyncio.get_running_loop()
            # Already inside an event loop — run in a thread pool
            import concurrent.futures
            with concurrent.futures.ThreadPoolExecutor() as pool:
                future = pool.submit(asyncio.run, _council_deliberate_async(config, query, context))
                deliberation = future.result(timeout=120)
        except RuntimeError:
            # No running loop — safe to call asyncio.run() directly
            deliberation = asyncio.run(_council_deliberate_async(config, query, context))

    except Exception as async_err:
        print(f'[wizard/obc-v2] Async path failed ({async_err}), falling back to sequential')
        # Sequential fallback
        seats = config.get('seats', {})
        responses = {}
        costs = {}
        models_used = {}
        for seat_name, seat_cfg in seats.items():
            try:
                _, text, cost, used = call_seat_sync(seat_name, seat_cfg, query, context)
                responses[seat_name] = text
                costs[seat_name] = cost
                models_used[seat_name] = used
            except Exception as seat_err:
                print(f'[wizard/obc-v2] Sequential seat {seat_name} failed: {seat_err}')
                responses[seat_name] = f'[error: {seat_err}]'
                costs[seat_name] = None
                models_used[seat_name] = 'unknown'

        # Sequential synthesis
        synth_cfg = config.get('synthesis', {})
        council_text = '\n\n'.join(
            f'## {k.replace("-", " ").title()}\n{v}' for k, v in responses.items()
        )
        synth_prompt = (
            f'Question: {query}\n\n{council_text}\n\n'
            'Synthesize into JSON: {recommendation, confidence, disagreements, summary}'
        )
        try:
            synth_text, synth_cost, _ = _call_litellm(
                synth_cfg.get('model', 'claude-sonnet-4-6'),
                [
                    {'role': 'system', 'content': 'You are Lumina synthesizing the Obsidian Circle deliberation.'},
                    {'role': 'user', 'content': synth_prompt},
                ],
                0.3,
                synth_cfg.get('max_tokens', 1000),
            )
            try:
                synthesis_data = json.loads(synth_text)
            except json.JSONDecodeError:
                synthesis_data = {'raw': synth_text}
        except Exception as se:
            synthesis_data = {'error': str(se)}

        deliberation = {
            'responses': responses,
            'models_used': models_used,
            'costs': costs,
            'synthesis': synthesis_data,
            'errors': {},
        }

    # Compute total cost
    total_cost = sum(v for v in deliberation.get('costs', {}).values() if v is not None)

    result = {
        'session_id': session_id,
        'query': query,
        'responses': deliberation['responses'],
        'models_used': deliberation['models_used'],
        'costs': deliberation['costs'],
        'synthesis': deliberation['synthesis'],
        'total_cost': total_cost,
        'status': 'complete',
    }

    print(f'[wizard/obc-v2] Deliberation {session_id[:8]} complete | cost=${total_cost:.4f}')

    # OBC-41: journal session to Engram (non-blocking)
    try:
        _journal_council_session(query, result)
    except Exception:
        pass  # Non-blocking

    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--query', required=True)
    parser.add_argument('--tier', default='auto', choices=['auto', 'gated'])
    parser.add_argument('--council', action='store_true')
    parser.add_argument('--parallel', action='store_true',
                        help='Use OBC v2 parallel multi-model deliberation')
    parser.add_argument('--context', default='')
    parser.add_argument('--cortex-files', default='',
                        help='Comma-separated file paths for Cortex code analysis (OBC-37)')
    args = parser.parse_args()

    # Load env from axon's .env (same credentials)
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip())

    # Refresh module globals after env load
    import __main__ as _m
    _m.INBOX_DB_HOST = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
    _m.INBOX_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
    _m.INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', '')
    _m.GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')

    if args.parallel:
        cortex_files = [f.strip() for f in args.cortex_files.split(',') if f.strip()] if args.cortex_files else None
        result = council_deliberate_parallel(args.query, args.context, cortex_files=cortex_files)
        print(json.dumps({k: v for k, v in result.items() if k != 'responses'}, indent=2))
        print('\n--- SYNTHESIS ---')
        print(json.dumps(result.get('synthesis', {}), indent=2))
    else:
        result = consult(args.query, args.tier, args.council, args.context)
        print(json.dumps({k: v for k, v in result.items() if k != 'result'}, indent=2))
        print('\n--- RESULT ---')
        print(result.get('result', ''))
