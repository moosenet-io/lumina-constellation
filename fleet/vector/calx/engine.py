"""
CalxEngine — orchestrates all trigger checks for a loop iteration.
Behavioral correction concepts adapted from getcalx/oss (archived).

SOM P5-19: Also reads soft rules from engram/system/calx/rules.json
(populated by Soma /api/insights accept endpoint) and injects them
as corrections.
"""
import os
import json
from pathlib import Path
from dataclasses import dataclass, field
from typing import Optional
from .triggers import T1TestTriggers, T2StyleTriggers, T3SecurityTriggers, T4PoisonedPromiseTriggers, TriggerResult, TriggerLevel


@dataclass
class IterationContext:
    """Snapshot of a loop iteration for Calx evaluation."""
    iteration: int
    test_output: str = ''
    lint_output: str = ''
    git_diff: str = ''
    changed_files: list[str] = field(default_factory=list)
    task_description: str = ''
    claimed_complete: bool = False


@dataclass
class CalxVerdict:
    """Result of Calx evaluation."""
    blocked: bool           # True = hard block, must fix before proceeding
    corrections: list[str]  # Soft corrections to inject into next prompt
    triggers_fired: list[TriggerResult] = field(default_factory=list)
    escalate: bool = False  # True when T4 fires twice → human review needed

    @property
    def has_corrections(self) -> bool:
        return bool(self.corrections or self.blocked)

    def correction_prompt(self) -> str:
        """Generate the correction text to inject into the next iteration prompt."""
        if not self.has_corrections:
            return ""
        parts = ["[CALX BEHAVIORAL CORRECTION]"]
        if self.blocked:
            parts.append("HARD BLOCK -- fix required before proceeding:")
        for trigger in self.triggers_fired:
            if trigger.correction:
                parts.append(f"* {trigger.correction}")
        return "\n".join(parts)


class CalxEngine:
    """Orchestrates trigger evaluation across all three tiers."""

    # Soma-derived rules file path (SOM P5-19)
    _SOMA_RULES_FILE = Path(os.environ.get('FLEET_DIR', '/opt/lumina-fleet')) / 'engram' / 'system' / 'calx' / 'rules.json'

    def __init__(self, history=None):
        self.t1 = T1TestTriggers()
        self.t2 = T2StyleTriggers()
        self.t3 = T3SecurityTriggers()
        self.t4 = T4PoisonedPromiseTriggers()
        self.history = history  # Optional CalxHistory for logging
        self._soma_rules: list[dict] = self._load_soma_rules()

    @classmethod
    def _load_soma_rules(cls) -> list[dict]:
        """Load soft rules from Soma insights (engram/system/calx/rules.json)."""
        try:
            if cls._SOMA_RULES_FILE.exists():
                rules = json.loads(cls._SOMA_RULES_FILE.read_text())
                return [r for r in rules if r.get('enabled', True)]
        except Exception:
            pass
        return []

    def reload_soma_rules(self):
        """Reload Soma rules from disk (call at start of each iteration)."""
        self._soma_rules = self._load_soma_rules()

    def evaluate(self, ctx: IterationContext) -> CalxVerdict:
        """Evaluate all triggers for this loop iteration."""
        all_triggers: list[TriggerResult] = []

        # T3 first — security blocks take priority
        all_triggers += self.t3.check_all(diff=ctx.git_diff)

        # T1 — test compliance
        all_triggers += self.t1.check_all(
            test_output=ctx.test_output,
            git_diff=ctx.git_diff
        )

        # T2 — style (only if no hard blocks)
        if not any(t.level == TriggerLevel.HARD for t in all_triggers):
            all_triggers += self.t2.check_all(
                lint_output=ctx.lint_output,
                changed_files=ctx.changed_files
            )

        # T4: poisoned promise check
        if ctx.claimed_complete:
            t4_results = self.t4.check_all(
                task_name=ctx.task_description,
                claimed_complete=True,
                test_output=ctx.test_output,
                lint_output=ctx.lint_output
            )
            all_triggers += t4_results

        blocked = any(t.level == TriggerLevel.HARD for t in all_triggers)
        corrections = [t.correction for t in all_triggers if t.correction]

        # SOM P5-19: Inject Soma-derived soft rules as additional corrections
        for rule in self._soma_rules:
            description = rule.get('description', '')
            action = rule.get('action', '')
            if description or action:
                corrections.append(f"[Soma rule: {rule.get('title', rule.get('id', ''))}] {description or action}")

        verdict = CalxVerdict(
            blocked=blocked,
            corrections=corrections,
            triggers_fired=all_triggers
        )

        # If T4 fires twice → auto-escalate
        if ctx.claimed_complete and self.t4.needs_escalation(ctx.task_description):
            verdict.escalate = True

        # Log to history if available
        if self.history and all_triggers:
            for trigger in all_triggers:
                self.history.log(
                    iteration=ctx.iteration,
                    trigger=trigger,
                    task_description=ctx.task_description
                )

        return verdict


def check_skill_proposals(history, skills_dir: str = None, min_count: int = 3) -> list[dict]:
    """VEC-93: Check if any Calx patterns have fired enough times to warrant a skill proposal.
    
    When a trigger fires >= min_count times for the same description, generate a skill
    proposal via Soma's skill_propose pipeline.
    
    Returns list of proposed skills: [{name, description, trigger_type, count}]
    """
    import os, sys
    from pathlib import Path

    if history is None:
        return []

    frequent = history.frequent_triggers(min_count=min_count)
    proposed = []

    for pattern in frequent:
        trigger_type = pattern['trigger_type']
        description = pattern['description']
        count = pattern['count']

        # Generate skill name from trigger type
        skill_name_map = {
            'T1_TEST': 'always-write-tests',
            'T2_STYLE': 'enforce-style-conventions',
            'T3_SECURITY': 'security-hardening',
            'T4_PROMISE': 'verify-before-completion',
        }
        skill_name = skill_name_map.get(trigger_type, f'calx-{trigger_type.lower().replace("_", "-")}')

        # Build skill content
        procedure = f"""## Observed Pattern

This skill was auto-generated because the following Calx trigger fired {count} times:
- **Trigger type:** {trigger_type}
- **Pattern:** {description}

## Procedure

Apply this correction proactively before the trigger fires:

"""
        if trigger_type == 'T1_TEST':
            procedure += """1. Before writing any code change, check if the changed functions have tests
2. Write tests first if they don't exist (TDD approach)
3. Run the test suite after every code change
4. Never delete a test without a replacement"""
        elif trigger_type == 'T2_STYLE':
            procedure += """1. Run linting before committing: `flake8 . --max-line-length=120`
2. Keep files under 500 lines — split if they grow larger
3. Keep functions under 50 lines — extract helpers if needed
4. Follow naming conventions from the project's conventions.md"""
        elif trigger_type == 'T3_SECURITY':
            procedure += """1. Never hardcode API keys, tokens, or passwords in code
2. Always use environment variables or a secrets manager
3. Use parameterized queries for all database operations
4. Add explicit exit conditions to all while loops"""

        # Try to propose via soma_skill_propose
        soma_dir = Path(os.environ.get('LUMINA_FLEET', '/opt/lumina-fleet')) / 'soma'
        propose_script = soma_dir / 'soma_skill_propose.py'

        proposal = {
            'skill_name': skill_name,
            'description': f'Auto-generated from {count} {trigger_type} trigger activations',
            'trigger_type': trigger_type,
            'count': count,
            'proposed': False,
        }

        if propose_script.exists():
            try:
                sys.path.insert(0, str(soma_dir))
                import soma_skill_propose as ssp
                # Import ssp module to use write_proposed_skill
                candidate = {
                    'name': skill_name,
                    'description': proposal['description'],
                    'tool_sequence': [f'calx_{trigger_type.lower()}'],
                    'occurrences': count,
                    'domain': trigger_type.lower(),
                }
                ssp._write_proposed_skill(candidate)
                proposal['proposed'] = True
            except Exception as e:
                proposal['error'] = str(e)

        proposed.append(proposal)

    return proposed
