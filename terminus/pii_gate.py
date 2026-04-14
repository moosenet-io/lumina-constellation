"""
pii_gate.py — MCP tool chain PII scanner
Terminus (MCP hub) · Lumina Constellation

Built-in pattern library + operator config from /opt/lumina-fleet/security/pii-patterns.yaml.
Wrap any MCP tool that sends content externally with gate() before returning.

Usage:
    from pii_gate import gate, scan

    # In a tool function:
    ok, msg = gate('gitea_create_file', content)
    if not ok:
        return msg  # return the block message to the LLM
"""

import re
import os
from pathlib import Path
from typing import Optional

try:
    import yaml
    _YAML = True
except ImportError:
    _YAML = False

# ── Config ──────────────────────────────────────────────────────────────────

CONFIG_PATH = os.environ.get(
    'PII_CONFIG_PATH',
    '/opt/lumina-fleet/security/pii-patterns.yaml'
)

# ── Built-in Patterns ────────────────────────────────────────────────────────

BUILTIN_PATTERNS = {
    'ipv4_private': (
        r'(?<!\d)'
        r'(192\.168\.\d{1,3}\.\d{1,3}'
        r'|10\.\d{1,3}\.\d{1,3}\.\d{1,3}'
        r'|172\.(1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3})'
        r'(?!\d)'
    ),
    'email_personal': (
        r'[a-zA-Z0-9._%+-]+'
        r'@(gmail|yahoo|outlook|hotmail|protonmail)\.(com|net|org)'
    ),
    'phone_us': (
        r'\+?1?[-.\s]?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}'
    ),
    'api_key_openai': r'sk-[a-zA-Z0-9]{20,}',
    'ssh_private_key': (
        r'-----BEGIN (RSA|DSA|EC|OPENSSH) PRIVATE KEY-----'
    ),
    'jwt_token': (
        r'eyJ[a-zA-Z0-9_-]{10,}\.eyJ[a-zA-Z0-9_-]{10,}'
    ),
    'aws_key': r'AKIA[0-9A-Z]{16}',
    'generic_secret': (
        r'(?i)(password|secret|token|apikey|api_key)'
        r'\s*[:=]\s*["\']?[a-zA-Z0-9/+]{16,}'
    ),
}

# ── Config loader ─────────────────────────────────────────────────────────────

def load_config() -> tuple:
    """Returns (operator_patterns: dict, allowlist: list)."""
    if not _YAML:
        return {}, []
    try:
        with open(CONFIG_PATH) as f:
            cfg = yaml.safe_load(f) or {}
        return cfg.get('patterns', {}), cfg.get('allowlist', [])
    except FileNotFoundError:
        return {}, []
    except Exception:
        return {}, []


# ── Scanner ───────────────────────────────────────────────────────────────────

def scan(content: str) -> list:
    """
    Scan content for PII/secrets.
    Returns list of findings: [{type, count, samples}]
    Empty list = clean.
    """
    operator_patterns, allowlist = load_config()
    all_patterns = {**BUILTIN_PATTERNS, **operator_patterns}

    # Apply allowlist — blank out intentional placeholder patterns
    scrubbed = content
    for allowed in allowlist:
        try:
            scrubbed = re.sub(allowed, '', scrubbed)
        except re.error:
            pass  # skip malformed allowlist patterns

    findings = []
    for name, pattern in all_patterns.items():
        if not pattern:
            continue
        try:
            matches = re.findall(pattern, scrubbed, re.IGNORECASE)
        except re.error:
            continue
        if matches:
            # Flatten tuples from groups
            flat = [m if isinstance(m, str) else ''.join(m) for m in matches]
            findings.append({
                'type': name,
                'count': len(flat),
                'samples': [s[:40] for s in flat[:3]],
            })
    return findings


# ── Gate ─────────────────────────────────────────────────────────────────────

def gate(tool_name: str, content: str) -> tuple:
    """
    PII gate for MCP tools that send content externally.

    Returns:
        (True, "Clean") — safe to proceed
        (False, "BLOCKED by PII gate: ...") — do NOT send this content

    Usage:
        ok, msg = gate('github_create_file', file_content)
        if not ok:
            return msg
    """
    if not content or not isinstance(content, str):
        return True, "Clean"

    findings = scan(content)
    if not findings:
        return True, "Clean"

    types = [f['type'] for f in findings]
    samples = []
    for f in findings[:3]:
        samples.extend(f['samples'][:1])

    detail = ', '.join(types)
    sample_str = ' | '.join(f'"{s}"' for s in samples[:3]) if samples else ''

    msg = (
        f"BLOCKED by PII gate in {tool_name}: "
        f"{detail} detected."
    )
    if sample_str:
        msg += f" Matches: {sample_str}."
    msg += " Remove sensitive content and retry."

    return False, msg


# ── Decorator ────────────────────────────────────────────────────────────────

def pii_gate(content_param: str = 'content'):
    """
    Decorator for MCP tool functions that send content externally.

    @pii_gate(content_param='file_content')
    def github_create_file(path: str, file_content: str, ...):
        ...
    """
    def decorator(func):
        def wrapper(*args, **kwargs):
            content = kwargs.get(content_param)
            if content:
                ok, msg = gate(func.__name__, content)
                if not ok:
                    return msg
            return func(*args, **kwargs)
        wrapper.__name__ = func.__name__
        wrapper.__doc__ = func.__doc__
        return wrapper
    return decorator
