"""
engine.py — Obsidian Circle convene() engine. (OC.1)

Multi-model reasoning council with ReAct loop, budget enforcement,
tool result broadcasting, and structured output validation.

Architecture:
  1. Load preset (members, default schema, synthesis model)
  2. For each member: ReAct loop → think → observe → form position
  3. Tool results broadcast to all subsequent members
  4. Synthesize positions via Mr. Wizard
  5. Validate against output_schema
  6. Evaluate confidence threshold
  7. Return typed result + action guidance
"""

import hashlib
import json
import os
import re
import sys
import time
import urllib.request
import urllib.error
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Optional

sys.path.insert(0, os.path.join(os.environ.get('FLEET_DIR', '/opt/lumina-fleet'), 'fleet'))

from .presets import resolve_preset
from .personas import get_persona
from .output import validate_output, evaluate_confidence

LITELLM_URL = os.environ.get('LITELLM_URL', '')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', os.environ.get('LITELLM_KEY', ''))
REQUEST_TIMEOUT = 45

# Rough cost per 1M tokens (input, output) in USD
_COST_TABLE = {
    'claude-opus-4-6':    (15.0, 75.0),
    'claude-sonnet-4-6':  (3.0,  15.0),
    'claude-haiku-4-5':   (0.25, 1.25),
    'Lumina':             (3.0,  15.0),     # Sonnet alias in LiteLLM
    'Lumina Fast':        (0.25, 1.25),     # Haiku alias in LiteLLM
    'openai/o3':          (10.0, 30.0),
    'google/gemini-2.0-flash-001': (0.10, 0.40),
}


def _call_litellm(model: str, messages: list, max_tokens: int = 800,
                  temperature: float = 0.3) -> dict:
    """
    Call LiteLLM proxy. Returns {content, input_tokens, output_tokens}.
    Raises RuntimeError if LITELLM_URL not set or call fails.
    """
    if not LITELLM_URL:
        raise RuntimeError('LITELLM_URL not set — cannot call council models')

    payload = json.dumps({
        'model': model,
        'messages': messages,
        'max_tokens': max_tokens,
        'temperature': temperature,
    }).encode()

    headers = {'Content-Type': 'application/json'}
    if LITELLM_KEY:
        headers['Authorization'] = f'Bearer {LITELLM_KEY}'

    req = urllib.request.Request(
        f'{LITELLM_URL}/chat/completions',
        data=payload, headers=headers
    )
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
            data = json.loads(resp.read())
    except urllib.error.HTTPError as e:
        body = e.read().decode()[:200]
        raise RuntimeError(f'LiteLLM HTTP {e.code}: {body}')

    content = data['choices'][0]['message']['content']
    usage = data.get('usage', {})
    return {
        'content': content,
        'input_tokens': usage.get('prompt_tokens', 0),
        'output_tokens': usage.get('completion_tokens', 0),
    }


def _estimate_cost(model: str, input_tokens: int, output_tokens: int) -> float:
    """Rough USD cost estimate for budget gating."""
    inp_cost, out_cost = _COST_TABLE.get(model, (3.0, 15.0))
    return (input_tokens * inp_cost + output_tokens * out_cost) / 1_000_000


def _extract_tag(text: str, tag: str) -> Optional[str]:
    """Extract content between <tag>…</tag>. Returns None if not found."""
    start = text.find(f'<{tag}>')
    end = text.find(f'</{tag}>')
    if start == -1 or end == -1:
        return None
    return text[start + len(tag) + 2:end]


def _member_react_loop(
    member: dict,
    question: str,
    tool_results: dict,
    tool_executor: Optional[Callable],
) -> dict:
    """
    Run a single council member's ReAct loop.

    member dict keys:
      id, model, persona_id, max_tokens (default 700), temperature (default 0.3)

    Returns:
      {member_id, model, position, reasoning, confidence, tokens_used, cost}
    """
    persona = get_persona(member.get('persona_id', 'pragmatist'))
    model = member.get('model', 'Lumina')
    max_tokens = member.get('max_tokens', 700)
    temperature = member.get('temperature', 0.3)

    messages = [{'role': 'system', 'content': persona['system_prompt']}]

    # Broadcast: share tool results from earlier members
    if tool_results:
        ctx_lines = ['Shared analysis from other council members:']
        for tool_name, result in tool_results.items():
            ctx_lines.append(f'\n[{tool_name}]:\n{result}')
        messages.append({'role': 'user', 'content': '\n'.join(ctx_lines)})
        messages.append({'role': 'assistant', 'content': 'Understood. I have reviewed the shared context.'})

    messages.append({'role': 'user', 'content': f"""Question for deliberation: {question}

Reason through this step by step from your specific perspective.

Respond in this exact format:

<reasoning>
[Your step-by-step analysis — be specific and concrete]
</reasoning>

<position>
[Your clear recommendation — 2-4 sentences. Be direct.]
</position>

<confidence>
[A number between 0.0 and 1.0 representing your confidence in this position]
</confidence>"""})

    try:
        result = _call_litellm(model, messages, max_tokens=max_tokens, temperature=temperature)
        content = result['content']
        in_tok = result['input_tokens']
        out_tok = result['output_tokens']

        reasoning = _extract_tag(content, 'reasoning') or ''
        position = _extract_tag(content, 'position') or content[:400]
        conf_str = _extract_tag(content, 'confidence') or '0.6'

        try:
            confidence = max(0.0, min(1.0, float(conf_str.strip())))
        except ValueError:
            confidence = 0.6

        cost = _estimate_cost(model, in_tok, out_tok)

        return {
            'member_id': member['id'],
            'model': model,
            'persona': persona['name'],
            'position': position.strip(),
            'reasoning': reasoning.strip(),
            'confidence': confidence,
            'tokens_used': in_tok + out_tok,
            'cost': cost,
        }

    except Exception as e:
        return {
            'member_id': member['id'],
            'model': model,
            'persona': member.get('persona_id', 'unknown'),
            'position': f'[Member error: {str(e)[:120]}]',
            'reasoning': '',
            'confidence': 0.0,
            'tokens_used': 0,
            'cost': 0.0,
            'error': str(e),
        }


def _synthesize(
    question: str,
    positions: list,
    output_schema: Optional[dict],
    synthesis_model: str = 'Lumina',
) -> dict:
    """
    Mr. Wizard synthesizes all member positions into a final recommendation.
    Returns {structured_output, synthesis, confidence, parse_error?}.
    """
    valid_positions = [p for p in positions if not p.get('error')]

    if not valid_positions:
        return {'structured_output': None, 'synthesis': 'No valid positions to synthesize', 'confidence': 0.0}

    position_block = '\n\n'.join(
        f"[{p['member_id']} ({p.get('persona', '?')}) — conf {p.get('confidence', '?'):.0%}]\n{p['position']}"
        for p in valid_positions
    )

    schema_instruction = ''
    if output_schema:
        schema_instruction = (
            f'\n\nYour response MUST be valid JSON matching this schema:\n'
            f'{json.dumps(output_schema, indent=2)}\n\n'
            f'Respond ONLY with the JSON object — no markdown, no explanation.'
        )

    prompt = f"""You are Mr. Wizard, the synthesis engine of the Obsidian Circle.

Question: {question}

Council positions:
{position_block}

Synthesize these into a final recommendation. Weight positions by confidence and reasoning quality.{schema_instruction}

{"Output JSON only." if output_schema else "Structure your synthesis as: 1) Consensus recommendation, 2) Key disagreements (if any), 3) Confidence (0.0-1.0), 4) One-paragraph executive summary."}"""

    messages = [
        {'role': 'system', 'content': 'You are Mr. Wizard, master synthesizer of multi-model AI deliberation. You distill multiple perspectives into clear, actionable recommendations.'},
        {'role': 'user', 'content': prompt}
    ]

    try:
        result = _call_litellm(synthesis_model, messages, max_tokens=1000, temperature=0.2)
        content = result['content']

        if output_schema:
            # Try direct parse
            try:
                parsed = json.loads(content.strip())
                confidence = float(parsed.get('confidence', 0.7))
                if confidence > 1.0:
                    confidence /= 10.0
                return {'structured_output': parsed, 'synthesis': content, 'confidence': confidence}
            except (json.JSONDecodeError, ValueError):
                pass
            # Find JSON block
            m = re.search(r'\{.*\}', content, re.DOTALL)
            if m:
                try:
                    parsed = json.loads(m.group())
                    confidence = float(parsed.get('confidence', 0.7))
                    if confidence > 1.0:
                        confidence /= 10.0
                    return {'structured_output': parsed, 'synthesis': content, 'confidence': confidence}
                except Exception:
                    pass
            return {'structured_output': None, 'synthesis': content, 'confidence': 0.3, 'parse_error': True}

        # No schema — extract confidence from text
        conf_match = re.search(r'[Cc]onfidence[:\s]+([0-9.]+)', content)
        confidence = 0.7
        if conf_match:
            try:
                c = float(conf_match.group(1))
                confidence = c if c <= 1.0 else c / 10.0
            except ValueError:
                pass

        return {'structured_output': None, 'synthesis': content, 'confidence': confidence}

    except Exception as e:
        return {'structured_output': None, 'synthesis': f'Synthesis failed: {e}', 'confidence': 0.0, 'error': str(e)}


# ── Session checkpointing (OC.5) ─────────────────────────────────────────────

_CHECKPOINT_DIR = Path(os.environ.get('FLEET_DIR', '/opt/lumina-fleet')) / 'engram' / 'council-sessions'
_CHECKPOINT_TTL_HOURS = 24


def _session_hash(question: str, circle: str) -> str:
    """Stable 16-char ID for a (question, circle) pair — used for checkpoint files."""
    return hashlib.sha256(f'{circle}:{question}'.encode()).hexdigest()[:16]


def _load_checkpoint(session_id: str) -> Optional[dict]:
    """Load a saved checkpoint. Returns None if missing or expired (>24h)."""
    path = _CHECKPOINT_DIR / f'{session_id}.json'
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text())
        saved_at_str = data.get('saved_at', '')
        if saved_at_str:
            saved_at = datetime.fromisoformat(saved_at_str)
            age_hours = (datetime.now(timezone.utc) - saved_at).total_seconds() / 3600
            if age_hours > _CHECKPOINT_TTL_HOURS:
                path.unlink(missing_ok=True)
                return None
        return data
    except Exception:
        return None


def _save_checkpoint(session_id: str, data: dict):
    """Persist checkpoint after each member completes. Non-fatal on failure."""
    try:
        _CHECKPOINT_DIR.mkdir(parents=True, exist_ok=True)
        data['saved_at'] = datetime.now(timezone.utc).isoformat()
        (_CHECKPOINT_DIR / f'{session_id}.json').write_text(json.dumps(data, indent=2))
    except Exception:
        pass


def _clear_checkpoint(session_id: str):
    """Remove checkpoint once deliberation is complete."""
    try:
        (_CHECKPOINT_DIR / f'{session_id}.json').unlink(missing_ok=True)
    except Exception:
        pass


def convene(
    question: str,
    circle: str = 'quick',
    tools: Optional[list] = None,
    output_schema: Optional[dict] = None,
    budget: float = 0.10,
    mode: str = 'multi',
    tool_executor: Optional[Callable] = None,
    resume: bool = True,
) -> dict:
    """
    Convene the Obsidian Circle for multi-model deliberation.

    Args:
        question:       The question, decision, or problem to deliberate on.
        circle:         Preset name — quick/architecture/security/cost/research/full/custom.
        tools:          MCP tool names the council may reference (informational).
        output_schema:  JSON schema for structured output validation.
        budget:         Max USD to spend (default $0.10).
        mode:           'multi' (different models) or 'prism' (same model, diff personas).
        tool_executor:  Optional callable(tool_name, params) -> str for live tool calls.

    Returns dict:
        result          Structured output (if schema) or synthesis text
        confidence      0.0–1.0 float
        action          'auto_act' | 'ask_operator' | 'surface_deliberation'
        positions       List of member position dicts
        synthesis       Raw synthesis text from Mr. Wizard
        cost_usd        Total spend estimate
        elapsed_s       Wall time in seconds
        circle          Preset name used
        mode            Mode used
        member_count    Number of members who responded
        deliberation_log  Full deliberation for review/archiving
        resumed         True if this session resumed from a checkpoint
    """
    t_start = time.time()

    preset = resolve_preset(circle)
    members = preset['members']
    schema = output_schema or preset.get('default_schema')
    synthesis_model = preset.get('synthesis_model', 'Lumina')

    # Prism mode: force all members to same model, different personas
    if mode == 'prism':
        prism_model = preset.get('prism_model', 'Lumina')
        members = [dict(m, model=prism_model) for m in members]

    # Checkpoint resume (OC.5)
    session_id = _session_hash(question, circle)
    checkpoint = _load_checkpoint(session_id) if resume else None
    resumed = False

    if checkpoint and checkpoint.get('circle') == circle:
        positions = checkpoint.get('positions', [])
        tool_results = checkpoint.get('tool_results', {})
        start_index = len(positions)
        if start_index > 0:
            resumed = True
    else:
        positions = []
        tool_results = {}
        start_index = 0

    total_cost = sum(p.get('cost', 0.0) for p in positions)

    for i, member in enumerate(members):
        if i < start_index:
            continue  # Already completed in prior session
        # Hard budget gate
        if total_cost >= budget:
            positions.append({
                'member_id': member['id'],
                'position': '[Budget exhausted — member skipped]',
                'error': 'budget_exhausted',
                'cost': 0.0,
                'tokens_used': 0,
                'confidence': 0.0,
            })
            continue

        remaining = budget - total_cost
        # Rough token cap: remaining budget / $0.000003 per token (Sonnet rate)
        budget_token_cap = max(100, int(remaining / 0.000003))
        member_copy = dict(member)
        member_copy['max_tokens'] = min(member.get('max_tokens', 700), budget_token_cap)

        pos = _member_react_loop(
            member=member_copy,
            question=question,
            tool_results=tool_results,
            tool_executor=tool_executor,
        )
        positions.append(pos)
        total_cost += pos.get('cost', 0.0)

        # Save checkpoint after each member (OC.5)
        _save_checkpoint(session_id, {
            'question': question,
            'circle': circle,
            'mode': mode,
            'positions': positions,
            'tool_results': tool_results,
        })

        # Broadcast tool results (future: parse tool calls from reasoning)
        # For now: if tool_executor is provided, any member can add results to tool_results
        # via returned 'tool_calls' key (forward-compatible hook)
        if tool_executor and pos.get('tool_calls'):
            for tool_call in pos['tool_calls']:
                try:
                    tr = tool_executor(tool_call['name'], tool_call.get('params', {}))
                    tool_results[tool_call['name']] = str(tr)
                except Exception:
                    pass

    # Clear checkpoint — deliberation complete (OC.5)
    _clear_checkpoint(session_id)

    # Synthesis
    synthesis = _synthesize(question, positions, schema, synthesis_model)
    total_cost += _estimate_cost(synthesis_model, 400, 600)

    # Validate structured output
    validated_output = None
    if schema and synthesis.get('structured_output'):
        validated_output, validation_errors = validate_output(synthesis['structured_output'], schema)
        if validation_errors:
            synthesis['validation_errors'] = validation_errors

    # Confidence + action
    raw_confidence = synthesis.get('confidence', 0.7)
    if isinstance(raw_confidence, (int, float)) and raw_confidence > 1.0:
        raw_confidence = raw_confidence / 10.0
    raw_confidence = max(0.0, min(1.0, float(raw_confidence)))

    action = evaluate_confidence(raw_confidence)
    elapsed = round(time.time() - t_start, 1)

    return {
        'result': validated_output or synthesis.get('synthesis', ''),
        'confidence': raw_confidence,
        'action': action,
        'positions': positions,
        'synthesis': synthesis.get('synthesis', ''),
        'cost_usd': round(total_cost, 6),
        'elapsed_s': elapsed,
        'circle': circle,
        'mode': mode,
        'member_count': len([p for p in positions if not p.get('error')]),
        'deliberation_log': {
            'question': question,
            'preset': circle,
            'positions': positions,
            'synthesis_raw': synthesis,
            'schema': schema,
        },
        'resumed': resumed,
        'session_id': session_id,
    }
