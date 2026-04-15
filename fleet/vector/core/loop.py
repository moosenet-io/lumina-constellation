"""
VectorLoop — core loop engine. Mode-agnostic.
Calls interface methods only — never imports backend implementations.
"""

import sys
import json
import time
import logging
from typing import Optional
from dataclasses import dataclass
import subprocess
import os

log = logging.getLogger('vector.loop')

# Calx behavioral correction — import is best-effort (graceful degradation if not installed)
try:
    import sys as _sys
    import os as _os
    _calx_path = _os.path.join(_os.path.dirname(_os.path.dirname(__file__)))
    if _calx_path not in _sys.path:
        _sys.path.insert(0, _calx_path)
    from calx import CalxEngine, CalxHistory
    from calx.engine import IterationContext
    _CALX_AVAILABLE = True
except ImportError:
    _CALX_AVAILABLE = False
    log.debug('Calx not available — behavioral correction disabled')

MAX_CONSECUTIVE_FAILURES = 3
MAX_ITERATIONS = 10


@dataclass
class LoopConfig:
    max_iterations: int = MAX_ITERATIONS
    max_cost: float = 5.00
    llm_url: str = 'http://YOUR_LITELLM_IP:4000'
    llm_key: str = ''
    llm_model: str = 'claude-sonnet-4-6'
    repo_path: str = '.'
    interactive: bool = False


class VectorLoop:
    """
    The mode-agnostic core loop.
    Receives interface implementations — never imports backends directly.
    """

    def __init__(self, state, bus, memory, cost, config: LoopConfig = None):
        self.state = state       # StateBackend
        self.bus = bus           # MessageBus
        self.memory = memory     # MemoryStore
        self.cost = cost         # CostGate
        self.config = config or LoopConfig()
        self._consecutive_failures = {}  # task_id -> failure count

        # Gap 1: execution delegator
        from core.delegator import ExecutionDelegator
        self.delegator = ExecutionDelegator(config.__dict__ if hasattr(config, '__dict__') else {})

        # Gap 3: context manager
        from core.context_manager import ContextManager
        self.ctx_mgr = ContextManager(
            model=getattr(config, 'llm_model', 'claude-sonnet'),
            initial_prime_pct=35,
        )

        # Gap 4: guardrails
        from guardrails import GuardrailsManager
        self.guardrails = GuardrailsManager(memory_store=memory, project_name='')
        self.guardrails.load()

        # Calx behavioral correction engine
        if _CALX_AVAILABLE:
            self.calx = CalxEngine(CalxHistory())
            log.debug('Calx behavioral correction enabled')
        else:
            self.calx = None

    def run(self, work_order: dict = None) -> dict:
        """Main entry point. Accepts work order or reads from MessageBus."""
        log.info('VectorLoop starting')

        # Phase 1: Intake
        if work_order is None:
            messages = self.bus.read(limit=1)
            if not messages:
                return {'status': 'idle', 'message': 'No work orders in inbox'}
            work_order = messages[0].get('payload', {})
            if isinstance(work_order, str):
                work_order = json.loads(work_order)
            self.bus.ack([messages[0].get('id', '')])

        task_desc = work_order.get('task', work_order.get('description', str(work_order)))
        log.info(f'Work order: {task_desc[:80]}')

        # Load conventions from memory
        conventions = self.memory.get_conventions()
        prior_context = self.memory.query(task_desc, top_k=3)

        # Phase 2: Plan
        tasks = self._plan(task_desc, conventions, prior_context)
        if not tasks:
            self.bus.send('lumina', 'escalation', {'reason': 'Planning failed — could not decompose task', 'task': task_desc})
            return {'status': 'escalated', 'reason': 'planning_failed'}

        # Phase 1.5: Cortex pre-flight — scope context to blast radius
        cortex_context = self._cortex_preflight(work_order.get('repo', 'lumina-fleet'), task_desc)

        completed = []
        total_cost = 0.0

        for task in tasks:
            # Create task in state backend
            task = self.state.create_task(task)
            log.info(f'Task: {task.name}')
            # Attach cortex context to task for executor
            if cortex_context:
                task.metadata['cortex_blast_radius'] = cortex_context.get('blast_radius', [])
                task.metadata['cortex_context'] = cortex_context

            iteration = 0
            while iteration < self.config.max_iterations:
                iteration += 1

                # Phase 3: Execute
                if not self.cost.check_budget(0.05):  # estimate per iteration
                    reason = f'Budget exhausted after {total_cost:.2f} spent'
                    approved = self.cost.request_approval(reason, 2.00)
                    if not approved:
                        self._escalate('budget_exceeded', task, reason)
                        return {'status': 'escalated', 'reason': 'budget_exceeded', 'spent': total_cost}

                self.state.update_status(task.id, 'running')
                exec_result = self._execute(task)
                spend = exec_result.get('cost', 0.02)
                self.cost.record_spend(spend)
                total_cost += spend

                if exec_result.get('error'):
                    failures = self._consecutive_failures.get(task.id, 0) + 1
                    self._consecutive_failures[task.id] = failures
                    if failures >= MAX_CONSECUTIVE_FAILURES:
                        self._escalate('repeated_failure', task, exec_result['error'])
                        self.state.update_status(task.id, 'escalated')
                        break
                    log.warning(f'Execution failed (attempt {failures}): {exec_result["error"][:60]}')
                    continue

                self._consecutive_failures[task.id] = 0

                # Phase 3.5: Cortex post-flight — risk score before accepting result
                if exec_result.get('file'):
                    repo = work_order.get('repo', 'lumina-fleet')
                    cortex_review = self._cortex_postflight(repo, [exec_result['file']])
                    risk_score = cortex_review.get('risk_score', 0)
                    if risk_score > 7:
                        log.warning(f'Cortex risk score {risk_score}/10 — escalating to Mr. Wizard')
                        self._escalate('high_risk_change', task,
                                       f'Risk score {risk_score}/10. Signals: {cortex_review.get("risk_signals", [])}')
                        self.state.update_status(task.id, 'escalated')
                        break
                    elif risk_score >= 5:
                        log.info(f'Cortex risk score {risk_score}/10 — proceeding with caution')
                        exec_result['cortex_risk'] = risk_score

                # Phase 3.6: Calx behavioral correction — pure-Python trigger checks
                calx_correction_prefix = ''
                if self.calx:
                    self.calx.reload_soma_rules()  # SOM P5-19: refresh Soma-derived rules
                    # Gather git diff for trigger analysis (best-effort)
                    _git_diff = ''
                    try:
                        _r = subprocess.run('git diff HEAD', shell=True, capture_output=True,
                                            text=True, timeout=10, cwd=self.config.repo_path)
                        _git_diff = _r.stdout if _r.returncode == 0 else ''
                    except Exception:
                        pass

                    _ctx = IterationContext(
                        iteration=iteration,
                        test_output=exec_result.get('test_output', ''),
                        lint_output=exec_result.get('lint_output', ''),
                        git_diff=_git_diff,
                        changed_files=[exec_result['file']] if exec_result.get('file') else [],
                        task_description=task.name,
                    )
                    _verdict = self.calx.evaluate(_ctx)

                    if _verdict.blocked:
                        log.warning(f'Calx HARD BLOCK on iteration {iteration}: '
                                    f'{[t.description for t in _verdict.triggers_fired]}')
                        # Store the correction for the next iteration's execute prompt
                        task.metadata['calx_correction'] = _verdict.correction_prompt()
                        continue  # skip review, retry execute with correction injected

                    if _verdict.has_corrections:
                        log.info(f'Calx soft corrections on iteration {iteration}: '
                                 f'{len(_verdict.corrections)} trigger(s)')
                        calx_correction_prefix = _verdict.correction_prompt()

                # Phase 4-5: Test + Review
                review = self._review(task, exec_result, calx_prefix=calx_correction_prefix)

                if review['decision'] == 'proceed':
                    self.state.complete_task(task.id, exec_result)
                    completed.append({'task': task.name, 'result': exec_result.get('summary', '')})
                    log.info(f'Task complete: {task.name}')
                    break
                elif review['decision'] == 'escalate':
                    self._escalate('ambiguity', task, review['reason'])
                    self.state.update_status(task.id, 'escalated')
                    break
                else:  # iterate
                    log.info(f'Iterating: {review.get("reason", "")}')

        # Phase 7-8: Complete + Learn
        result = {'status': 'complete', 'tasks_completed': len(completed), 'tasks': completed, 'cost': total_cost}
        self.bus.send('lumina', 'result', result)
        self.memory.store(f'vector/session/{int(time.time())}',
                         f'Completed: {task_desc[:50]}. Tasks: {len(completed)}. Cost: ${total_cost:.2f}')

        # VEC-95: Skill feedback — update usage metadata for skills that were loaded
        total_iterations = sum(t.get('iterations', 1) for t in completed)
        self._update_skill_feedback(task_desc, success=len(completed) > 0,
                                    iterations=total_iterations)

        # VEC-96: Auto-create skill proposal if task was complex and no skill matched
        if len(completed) > 0 and total_iterations >= 5:
            skill_context = self._discover_skills(task_desc)
            if not skill_context:
                self._propose_skill_from_task(task_desc, completed, total_iterations, total_cost)

        log.info(f'Loop complete: {len(completed)} tasks, ${total_cost:.2f}')
        return result

    def _propose_skill_from_task(self, task_desc: str, completed: list, iterations: int, cost: float):
        """VEC-96: Propose a new skill when a complex task completes with no existing skill match.
        Uses soma_skill_propose.py pipeline — goes to proposed/ for the operator's approval."""
        import os, sys
        from pathlib import Path
        soma_dir = Path(os.environ.get('LUMINA_FLEET', '/opt/lumina-fleet')) / 'soma'
        if not soma_dir.exists():
            return
        try:
            sys.path.insert(0, str(soma_dir))
            import soma_skill_propose as ssp
            tool_sequence = [t.get('task', '')[:30] for t in completed[:10]]
            candidate = {
                'name': task_desc[:30].lower().replace(' ', '-').strip('-'),
                'description': f'Auto-generated from {iterations}-iteration task: {task_desc[:80]}',
                'tool_sequence': tool_sequence,
                'occurrences': 1,
                'domain': 'general-automation',
            }
            ssp._write_proposed_skill(candidate)
            log.info(f'Proposed skill from complex task: {candidate["name"]} ({iterations} iterations)')
        except Exception as e:
            log.debug(f'Skill proposal failed (non-critical): {e}')

    def _update_skill_feedback(self, task_desc: str, success: bool, iterations: int = 1):
        """VEC-95: Update skill usage metadata after task completion."""
        import os, re, sys
        from pathlib import Path
        skills_dir = Path(os.environ.get('SKILLS_DIR', '/opt/lumina-fleet/skills/active'))
        shared_dir = Path(os.environ.get('LUMINA_SHARED', '/opt/lumina-fleet/shared'))
        tracker_path = shared_dir / 'skill_tracker.py'
        if not tracker_path.exists() or not skills_dir.exists():
            return
        try:
            task_words = set(re.findall(r'\b\w{4,}\b', task_desc.lower()))
            for skill_dir in skills_dir.iterdir():
                skill_file = skill_dir / 'SKILL.md'
                if not skill_dir.is_dir() or not skill_file.exists():
                    continue
                content = skill_file.read_text()
                skill_words = set(re.findall(r'\b\w{4,}\b', content[:200].lower()))
                if len(task_words & skill_words) > 0:
                    sys.path.insert(0, str(shared_dir))
                    import skill_tracker
                    if success:
                        skill_tracker.record_success(skill_dir.name)
                        log.info(f'Skill feedback: recorded success for {skill_dir.name}')
                    else:
                        skill_tracker.record_failure(skill_dir.name, f'Task failed after {iterations} iterations')
        except Exception as e:
            log.debug(f'Skill feedback update failed (non-critical): {e}')

    def _discover_skills(self, task_desc: str) -> str:
        """VEC-94: Load relevant skills from skills directory (agentskills.io).
        Keyword-based matching — no LLM cost. Max 2 skills loaded per task.
        Returns skill content to inject into the plan prompt, or empty string."""
        import os, re
        from pathlib import Path
        skills_dir = Path(os.environ.get('SKILLS_DIR', '/opt/lumina-fleet/skills/active'))
        if not skills_dir.exists():
            return ''
        task_words = set(re.findall(r'\b\w{4,}\b', task_desc.lower()))
        matches = []
        for skill_dir in sorted(skills_dir.iterdir()):
            skill_file = skill_dir / 'SKILL.md'
            if not skill_dir.is_dir() or not skill_file.exists():
                continue
            try:
                content = skill_file.read_text()
                skill_words = set()
                for pattern in [r'^name:\s*(.+)', r'^description:\s*(.+)', r'^tags:\s*\[(.+)\]']:
                    m = re.search(pattern, content, re.MULTILINE)
                    if m:
                        skill_words.update(re.findall(r'\b\w{3,}\b', m.group(1).lower()))
                score = len(task_words & skill_words)
                if score > 0:
                    matches.append((score, skill_dir.name, content))
            except Exception:
                continue
        if not matches:
            return ''
        matches.sort(reverse=True)
        loaded = []
        for score, name, content in matches[:2]:
            log.info(f'Skill loaded for planning: {name} (keyword score={score})')
            loaded.append(f'[SKILL: {name}]\n{content[:600]}')
        return '\n\n'.join(loaded)

    def _plan(self, task_desc: str, conventions: str, prior_context: list) -> list:
        """Phase 2: decompose task into atomic subtasks. Skill-aware (VEC-94)."""
        from backends.interfaces import Task
        import urllib.request

        # Load relevant skills into planning context (keyword-based, free)
        skill_context = self._discover_skills(task_desc)

        context_str = '\n'.join(prior_context[:2]) if prior_context else ''
        skill_section = f'\nRelevant skills:\n{skill_context[:800]}' if skill_context else ''

        # Gap 4: inject guardrails context before the LLM call
        guardrails_ctx = self.guardrails.get_context()

        guardrails_prefix = guardrails_ctx + '\n\n' if guardrails_ctx else ''
        prompt = f"""{guardrails_prefix}Break this development task into 3-7 atomic subtasks, each completable in one code change.

Task: {task_desc}

Conventions: {conventions[:500] if conventions else 'None'}

Prior context: {context_str[:300] if context_str else 'None'}{skill_section}

Return a numbered list of subtasks. Each on its own line starting with a number."""

        try:
            data = json.dumps({'model': self.config.llm_model, 'messages': [{'role':'user','content':prompt}], 'max_tokens':400}).encode()
            req = urllib.request.Request(f'{self.config.llm_url}/v1/chat/completions', data=data,
                headers={'Authorization': f'Bearer {self.config.llm_key}', 'Content-Type':'application/json'}, method='POST')
            with urllib.request.urlopen(req, timeout=30) as r:
                resp = json.load(r)['choices'][0]['message']['content']
        except Exception as e:
            log.error(f'Planning LLM call failed: {e}')
            return []

        tasks = []
        for line in resp.strip().split('\n'):
            line = line.strip()
            if line and line[0].isdigit():
                name = line.lstrip('0123456789.). ').strip()
                if name:
                    tasks.append(Task(name=name, description=name, status='queued',
                                    metadata={'parent_task': task_desc}))
        return tasks

    def _execute(self, task) -> dict:
        """Phase 3: attempt code change for task."""
        import urllib.request, subprocess, os

        # Gap 1/2: select model tier for this task
        model, tier = self.delegator.select_model(task.name, task.description if hasattr(task, 'description') else '')
        llm_model_for_task = model  # use instead of self.config.llm_model
        log.debug(f'_execute: tier={tier} model={llm_model_for_task} task={task.name[:40]}')

        # Prepend any pending Calx correction from a previous iteration
        calx_preamble = ''
        if task.metadata.get('calx_correction'):
            calx_preamble = task.metadata.pop('calx_correction') + '\n\n'

        prompt = f"""{calx_preamble}You are a software developer. Complete this atomic development task:

Task: {task.name}
Repo: {self.config.repo_path}

Write the minimal code change needed. Return:
1. Which file to modify (or create)
2. The exact content to write
3. How to verify it works (test command)

Format:
FILE: path/to/file.py
CONTENT:
```
[code here]
```
TEST: [command to verify]"""

        try:
            data = json.dumps({'model': llm_model_for_task, 'messages': [{'role':'user','content':prompt}], 'max_tokens':1000}).encode()
            req = urllib.request.Request(f'{self.config.llm_url}/v1/chat/completions', data=data,
                headers={'Authorization': f'Bearer {self.config.llm_key}', 'Content-Type':'application/json'}, method='POST')
            with urllib.request.urlopen(req, timeout=60) as r:
                response = json.load(r)['choices'][0]['message']['content']
        except Exception as e:
            return {'error': str(e)}

        # Parse response
        file_path = ''
        content = ''
        test_cmd = ''
        lines = response.split('\n')
        in_content = False
        for line in lines:
            if line.startswith('FILE:'):
                file_path = line[5:].strip()
            elif line.startswith('TEST:'):
                test_cmd = line[5:].strip()
                in_content = False
            elif line.strip() == 'CONTENT:':
                in_content = True
            elif in_content and not line.startswith('```'):
                content += line + '\n'

        # Apply change if file specified
        if file_path and content:
            full_path = os.path.join(self.config.repo_path, file_path)
            try:
                os.makedirs(os.path.dirname(full_path), exist_ok=True)
                with open(full_path, 'w') as f:
                    f.write(content.strip())
            except Exception as e:
                return {'error': f'File write failed: {e}', 'file': file_path}

        # Run test if specified
        test_output = ''
        test_passed = True
        if test_cmd:
            try:
                result = subprocess.run(test_cmd, shell=True, capture_output=True, text=True,
                                       timeout=30, cwd=self.config.repo_path)
                test_output = result.stdout + result.stderr
                test_passed = result.returncode == 0
            except Exception as e:
                test_output = str(e)
                test_passed = False

        return {
            'file': file_path, 'content_written': bool(content),
            'test_cmd': test_cmd, 'test_passed': test_passed,
            'test_output': test_output[:500], 'response': response[:300],
            'summary': f'Modified {file_path}' if file_path else 'Analysis complete',
            'cost': 0.05
        }

    def _review(self, task, exec_result: dict, calx_prefix: str = '') -> dict:
        """Phase 5: assess quality, decide next action."""
        import urllib.request

        if not exec_result.get('test_cmd'):
            return {'decision': 'proceed', 'reason': 'No test defined — accepting result'}
        if not exec_result.get('test_passed', True):
            return {'decision': 'iterate', 'reason': f'Test failed: {exec_result["test_output"][:100]}'}

        # Check memory for similar past decisions
        prior = self.memory.query(f'{task.name} quality review', top_k=2)

        # Prepend any Calx soft corrections to the review context
        calx_section = f'\nCalx corrections to address:\n{calx_prefix}\n' if calx_prefix else ''

        prompt = f"""Review this code change result. Decide: proceed, iterate, or escalate.

Task: {task.name}
File modified: {exec_result.get('file', 'none')}
Test passed: {exec_result.get('test_passed')}
Test output: {exec_result.get('test_output', '')[:200]}
Prior context: {chr(10).join(prior[:1])}{calx_section}

Reply with exactly one word: PROCEED, ITERATE, or ESCALATE
Then on the next line: reason (one sentence)"""

        try:
            data = json.dumps({'model': 'Lumina Fast', 'messages': [{'role':'user','content':prompt}], 'max_tokens':50}).encode()
            req = urllib.request.Request(f'{self.config.llm_url}/v1/chat/completions', data=data,
                headers={'Authorization': f'Bearer {self.config.llm_key}', 'Content-Type':'application/json'}, method='POST')
            with urllib.request.urlopen(req, timeout=20) as r:
                resp = json.load(r)['choices'][0]['message']['content'].strip()
        except Exception as e:
            return {'decision': 'proceed', 'reason': 'Review failed, accepting result'}

        lines = resp.split('\n')
        decision = lines[0].lower().strip().rstrip('.,!')
        reason = lines[1].strip() if len(lines) > 1 else ''

        if decision not in ('proceed', 'iterate', 'escalate'):
            decision = 'proceed'
        return {'decision': decision, 'reason': reason}

    def _cortex_preflight(self, repo: str, task_desc: str) -> dict:
        """Phase 1.5: Get blast radius from Cortex to scope Claude Code context."""
        try:
            import subprocess, json
            # Infer target files from task description (simple keyword match)
            target_files = []
            for word in task_desc.lower().split():
                if word.endswith('.py') or '/' in word:
                    target_files.append(word.strip('.,'))
            if not target_files:
                return {}  # No files to scope — skip

            cmd = f'python3 /opt/lumina-fleet/cortex/cortex.py blast {repo} {" ".join(target_files)}'
            r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=30)
            if r.returncode == 0:
                result = json.loads(r.stdout.strip())
                log.info(f'Cortex pre-flight: {result.get("blast_count", 0)} files in blast radius '
                         f'({result.get("token_reduction_pct", 0)}% token reduction)')
                return result
        except Exception as e:
            log.debug(f'Cortex pre-flight failed (non-blocking): {e}')
        return {}

    def _cortex_postflight(self, repo: str, changed_files: list) -> dict:
        """Phase 3.5: Get risk score from Cortex after executing a change."""
        try:
            import subprocess, json
            files_str = ' '.join(changed_files)
            cmd = f'python3 /opt/lumina-fleet/cortex/cortex.py review {repo} {files_str}'
            r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=30)
            if r.returncode == 0:
                result = json.loads(r.stdout.strip())
                risk = result.get('risk_score', 0)
                log.info(f'Cortex post-flight: risk score {risk}/10')
                return result
        except Exception as e:
            log.debug(f'Cortex post-flight failed (non-blocking): {e}')
        return {'risk_score': 0}

    def _escalate(self, reason_type: str, task, details: str):
        """Escalate an issue via MessageBus."""
        payload = {'reason': reason_type, 'task': task.name, 'details': details[:200]}
        self.bus.send('lumina', 'escalation', payload, priority='urgent')
        self.memory.store(f'vector/escalation/{int(time.time())}',
                         f'Escalated: {task.name} — {reason_type}: {details[:100]}')
        log.warning(f'Escalated: {reason_type} on {task.name}')
