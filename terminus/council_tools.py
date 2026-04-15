"""
council_tools.py — Obsidian Circle MCP tools for IronClaw. (OC.8)

Exposes convene() and session management as MCP-callable tools.
Refractor keyword category: council

Tools (4):
    council_convene   — Convene the circle, returns recommendation
    council_presets   — List available presets
    council_status    — Check a specific session or list recent
    council_history   — Full history table this runtime
"""

import json
import os
import sys
import time
from datetime import datetime, timezone

_FLEET_DIR = os.environ.get('FLEET_DIR', '/opt/lumina-fleet')
sys.path.insert(0, os.path.join(_FLEET_DIR, 'fleet'))

try:
    from obsidian_circle.engine import convene as _convene
    from obsidian_circle.presets import list_presets as _list_presets
    from obsidian_circle.output import format_for_operator as _format_for_operator, format_brief as _format_brief
    _OC_OK = True
except ImportError:
    _OC_OK = False

# Runtime session store (survives until MCP process restarts)
_SESSIONS: dict = {}


def register_council_tools(mcp):

    @mcp.tool()
    def council_convene(
        question: str,
        circle: str = 'quick',
        budget: float = 0.10,
        mode: str = 'multi',
        output_format: str = 'text',
    ) -> str:
        """Convene the Obsidian Circle for multi-model deliberation.

        The Circle is a multi-model reasoning council where different AI models and
        personas deliberate independently on a question, then synthesize a recommendation.

        Args:
            question:      The question, decision, or problem to deliberate on.
            circle:        Preset — quick (fast, 1 model), architecture (3 personas),
                           security (adversarial), cost (efficiency), research (4 models),
                           full (7 personas), custom.
            budget:        Max USD to spend. Default $0.10. quick ~ $0.01, full ~ $0.25.
            mode:          'multi' (different models) or 'prism' (same model, diff personas).
            output_format: 'text' for human-readable, 'json' for structured data.

        Returns:
            Council recommendation with confidence score and action guidance.
            action field: auto_act (>= 80%), ask_operator (50-80%), surface_deliberation (< 50%).

        Common uses:
            - Architectural decisions: circle='architecture'
            - Security review: circle='security'
            - Stuck Vector task: circle='quick' with task context
            - Complex reasoning: circle='research'
        """
        if not _OC_OK:
            return 'Obsidian Circle not available — obsidian_circle module not found at FLEET_DIR'

        session_id = f'oc_{int(time.time())}'

        try:
            result = _convene(
                question=question,
                circle=circle,
                budget=budget,
                mode=mode,
            )

            result['session_id'] = session_id
            _SESSIONS[session_id] = {
                'id': session_id,
                'timestamp': datetime.now(timezone.utc).isoformat(),
                'question': question,
                'circle': circle,
                'result': result,
            }

            if output_format == 'json':
                # Return JSON with deliberation log trimmed for token efficiency
                compact = {
                    'session_id': session_id,
                    'confidence': result.get('confidence'),
                    'action': result.get('action'),
                    'result': result.get('result'),
                    'cost_usd': result.get('cost_usd'),
                    'member_count': result.get('member_count'),
                }
                return json.dumps(compact, indent=2)
            else:
                return _format_for_operator(result)

        except Exception as e:
            return f'Council convene failed: {e}'

    @mcp.tool()
    def council_presets() -> str:
        """List all available Obsidian Circle presets with descriptions and member counts.

        Returns the 7 built-in presets plus any custom presets from constellation.yaml.
        Use this to pick the right circle for a deliberation.
        """
        if not _OC_OK:
            return 'Obsidian Circle not available'

        try:
            presets = _list_presets()
            lines = ['Obsidian Circle presets:', '']
            for p in presets:
                tag = ' [custom]' if p['source'] == 'custom' else ''
                lines.append(
                    f"  {p['name']:<14} {p['member_count']} member(s)  "
                    f"{p['description'][:65]}{tag}"
                )
            lines.append('')
            lines.append('Use: council_convene(question, circle="<name>")')
            return '\n'.join(lines)
        except Exception as e:
            return f'Error listing presets: {e}'

    @mcp.tool()
    def council_status(session_id: str = '') -> str:
        """Check status of a council session or list recent sessions.

        Args:
            session_id: Specific session ID from council_convene. Leave empty for recent list.

        Returns session details including question, circle, confidence, action, and cost.
        """
        if session_id:
            session = _SESSIONS.get(session_id)
            if not session:
                return f"Session '{session_id}' not found in this runtime"
            r = session['result']
            return (
                f"Session:    {session_id}\n"
                f"Question:   {session['question'][:100]}\n"
                f"Circle:     {session['circle']}\n"
                f"Members:    {r.get('member_count', 0)}\n"
                f"Confidence: {r.get('confidence', 0):.0%}\n"
                f"Action:     {r.get('action', 'unknown')}\n"
                f"Cost:       ${r.get('cost_usd', 0):.4f}\n"
                f"Elapsed:    {r.get('elapsed_s', 0)}s\n"
                f"Timestamp:  {session['timestamp']}\n"
            )

        if not _SESSIONS:
            return 'No council sessions this runtime'

        recent = sorted(_SESSIONS.values(), key=lambda s: s['timestamp'], reverse=True)[:8]
        lines = [f'Recent council sessions ({len(_SESSIONS)} total this runtime):', '']
        for s in recent:
            r = s['result']
            q_brief = s['question'][:55] + '...' if len(s['question']) > 55 else s['question']
            lines.append(
                f"  {s['id'][-12:]:<12}  [{s['circle']:<12}]  "
                f"conf={r.get('confidence', 0):.0%}  {r.get('action', '?'):<22}  {q_brief}"
            )
        return '\n'.join(lines)

    @mcp.tool()
    def council_history(limit: int = 10) -> str:
        """Return the history of council deliberations this runtime session.

        Shows a summary table of recent decisions with questions, presets, confidence,
        actions taken, and costs. Useful for reviewing deliberation patterns.

        Args:
            limit: Max number of sessions to return (default 10, max 50).
        """
        limit = min(limit, 50)

        if not _SESSIONS:
            return 'No council history this session'

        sessions = sorted(
            _SESSIONS.values(), key=lambda s: s['timestamp'], reverse=True
        )[:limit]

        lines = [
            f'Council history ({len(_SESSIONS)} total this runtime, showing {len(sessions)}):',
            '',
            f'  {"Session":<14} {"Circle":<13} {"Conf":<6} {"Action":<22} {"Cost":>8}  Question',
            '  ' + '-' * 90,
        ]

        for s in sessions:
            r = s['result']
            sid = s['id'][-12:]
            q = s['question'][:42] + '...' if len(s['question']) > 42 else s['question']
            lines.append(
                f"  {sid:<14} {s['circle']:<13} {r.get('confidence', 0):<6.0%} "
                f"{r.get('action', 'unknown'):<22} ${r.get('cost_usd', 0):>6.4f}  {q}"
            )

        total_cost = sum(s['result'].get('cost_usd', 0) for s in _SESSIONS.values())
        lines.append('')
        lines.append(f'  Total council spend this runtime: ${total_cost:.4f}')

        return '\n'.join(lines)
