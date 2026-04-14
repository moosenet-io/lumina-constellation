"""
Calx trigger definitions.
T1: Test compliance
T2: Style/conventions
T3: Anti-patterns (hard blocks)

All detection is pure Python — no LLM cost.
"""
import re
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Optional


class TriggerLevel(Enum):
    SOFT = "soft"    # Inject correction, continue
    HARD = "hard"    # Block commit until fixed


class TriggerType(Enum):
    T1_TEST = "T1_TEST"
    T2_STYLE = "T2_STYLE"
    T3_SECURITY = "T3_SECURITY"
    T4_PROMISE = "T4_PROMISE"


@dataclass
class TriggerResult:
    fired: bool
    trigger_type: TriggerType
    level: TriggerLevel
    description: str
    file: Optional[str] = None
    line: Optional[int] = None
    correction: str = ""  # Injected into next iteration prompt


class T1TestTriggers:
    """Test compliance triggers — fire when code quality degrades."""

    def check_test_failures(self, test_output: str) -> list[TriggerResult]:
        """T1-1: Tests failed."""
        results = []
        failure_patterns = [
            r'FAILED\s+', r'ERROR\s+', r'AssertionError', r'test_\w+ FAILED',
            r'\d+ failed', r'FAIL:', r'Error:.*test'
        ]
        for pattern in failure_patterns:
            if re.search(pattern, test_output, re.IGNORECASE):
                results.append(TriggerResult(
                    fired=True,
                    trigger_type=TriggerType.T1_TEST,
                    level=TriggerLevel.SOFT,
                    description="Test failures detected in test output",
                    correction="Tests are failing. Fix the failing tests before proceeding. "
                               "Do not skip or delete tests to make them pass."
                ))
                break
        return results

    def check_deleted_tests(self, git_diff: str) -> list[TriggerResult]:
        """T1-2: Tests deleted without replacement."""
        results = []
        deleted_test_lines = [l for l in git_diff.splitlines()
                               if l.startswith('-') and re.search(r'def test_', l)]
        added_test_lines = [l for l in git_diff.splitlines()
                             if l.startswith('+') and re.search(r'def test_', l)]
        if len(deleted_test_lines) > len(added_test_lines):
            results.append(TriggerResult(
                fired=True,
                trigger_type=TriggerType.T1_TEST,
                level=TriggerLevel.HARD,
                description=f"Tests deleted: {len(deleted_test_lines)} removed, {len(added_test_lines)} added",
                correction="You deleted tests without replacement. Add equivalent tests for any "
                           "deleted test coverage before proceeding. Do not reduce test coverage."
            ))
        return results

    def check_untested_functions(self, diff: str, test_output: str) -> list[TriggerResult]:
        """T1-3: New functions added without corresponding tests."""
        results = []
        new_funcs = re.findall(r'^\+\s*def\s+([a-z_][a-z0-9_]*)\s*\(', diff, re.MULTILINE)
        new_funcs = [f for f in new_funcs if not f.startswith('test_')]
        if new_funcs and 'no tests ran' in test_output.lower():
            results.append(TriggerResult(
                fired=True,
                trigger_type=TriggerType.T1_TEST,
                level=TriggerLevel.SOFT,
                description=f"New functions without tests: {new_funcs[:5]}",
                correction=f"You added new functions ({', '.join(new_funcs[:3])}) but no tests were run. "
                           "Write unit tests for new functions before committing."
            ))
        return results

    def check_all(self, test_output: str = '', git_diff: str = '') -> list[TriggerResult]:
        results = []
        if test_output:
            results += self.check_test_failures(test_output)
        if git_diff:
            results += self.check_deleted_tests(git_diff)
            if test_output:
                results += self.check_untested_functions(git_diff, test_output)
        return [r for r in results if r.fired]


class T2StyleTriggers:
    """Style and convention triggers."""

    def check_lint_output(self, lint_output: str) -> list[TriggerResult]:
        """T2-1: Linting failures."""
        results = []
        if lint_output and re.search(r'error|E\d{3}|F\d{3}', lint_output):
            results.append(TriggerResult(
                fired=True,
                trigger_type=TriggerType.T2_STYLE,
                level=TriggerLevel.SOFT,
                description="Linting errors detected",
                correction=f"Fix linting errors before committing:\n{lint_output[:500]}"
            ))
        return results

    def check_file_size(self, file_path: str, max_lines: int = 500) -> list[TriggerResult]:
        """T2-2: File too large."""
        results = []
        try:
            p = Path(file_path)
            if p.exists() and p.suffix == '.py':
                lines = len(p.read_text().splitlines())
                if lines > max_lines:
                    results.append(TriggerResult(
                        fired=True,
                        trigger_type=TriggerType.T2_STYLE,
                        level=TriggerLevel.SOFT,
                        description=f"{file_path} is {lines} lines (limit: {max_lines})",
                        file=file_path,
                        correction=f"{file_path} is {lines} lines, exceeding the {max_lines}-line limit. "
                                   "Split into smaller modules before proceeding."
                    ))
        except Exception:
            pass
        return results

    def check_function_length(self, file_path: str, max_lines: int = 50) -> list[TriggerResult]:
        """T2-3: Function too long."""
        results = []
        try:
            p = Path(file_path)
            if not p.exists() or p.suffix != '.py':
                return results
            content = p.read_text()
            # Find functions and approximate their lengths
            func_starts = [(m.start(), m.group(1)) for m in re.finditer(r'^def\s+(\w+)', content, re.MULTILINE)]
            lines = content.splitlines()
            for i, (pos, name) in enumerate(func_starts):
                start_line = content[:pos].count('\n')
                end_line = func_starts[i+1][0] if i+1 < len(func_starts) else len(lines)
                end_line = content[:end_line].count('\n') if i+1 < len(func_starts) else len(lines)
                func_lines = end_line - start_line
                if func_lines > max_lines:
                    results.append(TriggerResult(
                        fired=True,
                        trigger_type=TriggerType.T2_STYLE,
                        level=TriggerLevel.SOFT,
                        description=f"Function {name} in {file_path} is ~{func_lines} lines (limit: {max_lines})",
                        file=file_path,
                        correction=f"Function '{name}' is too long ({func_lines} lines). "
                                   f"Extract helper functions to keep each function under {max_lines} lines."
                    ))
        except Exception:
            pass
        return results

    def check_all(self, lint_output: str = '', changed_files: list[str] = None) -> list[TriggerResult]:
        results = []
        if lint_output:
            results += self.check_lint_output(lint_output)
        for f in (changed_files or []):
            results += self.check_file_size(f)
            results += self.check_function_length(f)
        return [r for r in results if r.fired]


class T3SecurityTriggers:
    """Anti-pattern triggers — HARD blocks. Must fix before proceeding."""

    # Patterns that look like real credentials (not placeholders/examples)
    SECRET_PATTERNS = [
        (r'(?i)(api_key|apikey|secret|token|password|passwd|pwd)\s*[=:]\s*["\'][a-zA-Z0-9_\-]{20,}["\']',
         "Possible hardcoded credential"),
        (r'ghp_[a-zA-Z0-9]{36}', "GitHub personal access token"),
        (r'sk-[a-zA-Z0-9]{48}', "OpenAI API key"),
        (r'xoxb-[0-9]{11}-[a-zA-Z0-9-]+', "Slack bot token"),
        (r'AKIA[A-Z0-9]{16}', "AWS access key"),
    ]

    SQL_INJECTION_PATTERNS = [
        r'f["\'].*SELECT.*{',     # f-string SQL
        r'f["\'].*INSERT.*{',
        r'f["\'].*UPDATE.*{',
        r'%.*SELECT',              # % string formatting in SQL
        r'".*SELECT.*" *%',
    ]

    def check_hardcoded_secrets(self, diff: str) -> list[TriggerResult]:
        """T3-1: Hardcoded credentials in diff."""
        results = []
        added_lines = [l[1:] for l in diff.splitlines() if l.startswith('+')]
        added_text = '\n'.join(added_lines)

        for pattern, desc in self.SECRET_PATTERNS:
            m = re.search(pattern, added_text)
            if m:
                # Skip obvious placeholders
                matched = m.group(0)
                if any(p in matched.lower() for p in ['example', 'placeholder', 'your_', 'changeme', 'xxx', 'test']):
                    continue
                results.append(TriggerResult(
                    fired=True,
                    trigger_type=TriggerType.T3_SECURITY,
                    level=TriggerLevel.HARD,
                    description=f"{desc} detected in diff",
                    correction=f"BLOCKED: {desc} detected. Remove the credential and use an "
                               "environment variable instead. Never commit real credentials."
                ))
        return results

    def check_sql_injection(self, diff: str) -> list[TriggerResult]:
        """T3-2: SQL injection patterns."""
        results = []
        added_text = '\n'.join(l[1:] for l in diff.splitlines() if l.startswith('+'))
        for pattern in self.SQL_INJECTION_PATTERNS:
            if re.search(pattern, added_text, re.IGNORECASE):
                results.append(TriggerResult(
                    fired=True,
                    trigger_type=TriggerType.T3_SECURITY,
                    level=TriggerLevel.HARD,
                    description="Possible SQL injection: string-formatted SQL detected",
                    correction="BLOCKED: SQL appears to use string formatting. "
                               "Use parameterized queries (cursor.execute(sql, params)) instead."
                ))
                break
        return results

    def check_unbounded_loops(self, diff: str) -> list[TriggerResult]:
        """T3-3: Unbounded loops (while True without break/return)."""
        results = []
        added_text = '\n'.join(l[1:] for l in diff.splitlines() if l.startswith('+'))
        while_true = re.findall(r'while\s+True\s*:', added_text)
        breaks = re.findall(r'\bbreak\b|\breturn\b|\braise\b', added_text)
        if len(while_true) > len(breaks):
            results.append(TriggerResult(
                fired=True,
                trigger_type=TriggerType.T3_SECURITY,
                level=TriggerLevel.SOFT,
                description=f"Unbounded loop detected: {len(while_true)} while True, {len(breaks)} exits",
                correction="while True loop without clear exit condition detected. "
                           "Add explicit break, return, or raise to prevent infinite loops."
            ))
        return results

    def check_all(self, diff: str = '') -> list[TriggerResult]:
        if not diff:
            return []
        results = (self.check_hardcoded_secrets(diff) +
                   self.check_sql_injection(diff) +
                   self.check_unbounded_loops(diff))
        return [r for r in results if r.fired]


class T4PoisonedPromiseTriggers:
    """T4: Poisoned promise — agent claims done but quality checks fail.
    Hard block: prevents false completion from propagating.
    """

    def __init__(self):
        self._t4_count_per_task: dict[str, int] = {}  # task_name → consecutive T4 count

    def check_false_completion(self, task_name: str, claimed_complete: bool,
                                test_output: str, lint_output: str) -> list[TriggerResult]:
        """T4: Loop review marks complete but checks still fail."""
        if not claimed_complete:
            self._t4_count_per_task.pop(task_name, None)
            return []

        # Check if quality gates actually passed
        failures = []
        if test_output and re.search(r'FAILED|ERROR.*test|AssertionError|\d+ failed', test_output, re.IGNORECASE):
            failures.append('tests are still failing')
        if lint_output and re.search(r'error|E\d{3}|F\d{3}', lint_output):
            failures.append('lint errors remain')

        if not failures:
            # Actually complete — reset counter
            self._t4_count_per_task.pop(task_name, None)
            return []

        # False completion detected
        count = self._t4_count_per_task.get(task_name, 0) + 1
        self._t4_count_per_task[task_name] = count

        failure_desc = ' and '.join(failures)
        trigger = TriggerResult(
            fired=True,
            trigger_type=TriggerType.T4_PROMISE,
            level=TriggerLevel.HARD,
            description=f'False completion: {failure_desc} (T4 count={count})',
            correction=(
                f'BLOCKED: You reported task completion but {failure_desc}. '
                f'Do NOT mark a task done until all quality checks pass. '
                f'Fix the failing checks before claiming completion.'
            )
        )

        if count >= 2:
            trigger.correction += (
                f' WARNING: This is the {count}nd consecutive false completion attempt. '
                f'Escalating to human review.'
            )

        return [trigger]

    def needs_escalation(self, task_name: str) -> bool:
        return self._t4_count_per_task.get(task_name, 0) >= 2

    def check_all(self, task_name: str = '', claimed_complete: bool = False,
                  test_output: str = '', lint_output: str = '') -> list[TriggerResult]:
        return self.check_false_completion(task_name, claimed_complete, test_output, lint_output)
