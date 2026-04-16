#!/usr/bin/env python3
"""Privacy scanner for internal Gitea commits and pushes.

Blocks private infrastructure details, secrets, and operator PII before code is
committed or pushed from any terminal checkout.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


MAX_FINDINGS = 80
MAX_SAMPLE = 120

ROOT = Path(__file__).resolve().parents[1]


SKIP_SUFFIXES = {
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".webp",
    ".ico",
    ".woff",
    ".woff2",
    ".ttf",
    ".otf",
    ".pdf",
    ".docx",
    ".sqlite",
    ".db",
    ".pyc",
}

SKIP_NAMES = {
    "rrweb-player.min.js",
    "rrweb.min.js",
}

SKIP_PATHS = {
    "scripts/privacy_scan.py",
}


ALLOWLIST: list[tuple[str, re.Pattern[str]]] = [
    ("generic placeholders", re.compile(r"YOUR_[A-Z0-9_]+(?:_IP|_HOST|_URL|_KEY|_TOKEN|_SECRET)?")),
    ("redaction words", re.compile(r"\b(?:REDACTED|CHANGEME|example\.com)\b", re.I)),
    ("public org", re.compile(r"(?:github\.com/)?moosenet-io\b", re.I)),
    ("standard cidr", re.compile(r"\b(?:10\.0\.0\.0/8|172\.16\.0\.0/12|192\.168\.0\.0/16|127\.0\.0\.0/8|169\.254\.0\.0/16)\b")),
    ("numeric strip chars", re.compile(r"0123456789\)\. ")),
]


PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    (
        "private_ipv4",
        re.compile(
            r"(?<![\d.])(?:"
            r"10\.(?:\d{1,3}\.){2}\d{1,3}|"
            r"172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}|"
            r"192\.168\.\d{1,3}\.\d{1,3}"
            r")(?![\d.])"
        ),
    ),
    ("container_id", re.compile(r"\bC[Tt]\d{3}\b")),
    ("proxmox_term", re.compile(r"\b(?:Proxmox|proxmox|PVE|PVS|PVM|pct\s+(?:exec|push|enter|start|stop|create))\b")),
    ("private_host_alias", re.compile(r"\b(?:pvs|pvm|pve)\b", re.I)),
    ("cluster_topology", re.compile(r"\b(?:3-node|three-node|virtualization cluster|proxmox cluster|multi-node cluster)\b", re.I)),
    ("openai_key", re.compile(r"\bsk-[A-Za-z0-9_-]{20,}\b")),
    ("aws_key", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("jwt_token", re.compile(r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b")),
    ("private_key", re.compile(r"-----BEGIN (?:RSA|DSA|EC|OPENSSH) PRIVATE KEY-----")),
    (
        "personal_email",
        re.compile(r"\b[A-Za-z0-9._%+-]+@(gmail|yahoo|outlook|hotmail|protonmail)\.(?:com|net|org)\b", re.I),
    ),
    ("us_phone", re.compile(r"\+?1?[-.\s]?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}\b")),
    (
        "secret_assignment",
        re.compile(
            r"(?i)\b(?:password|secret|token|api[_-]?key)\b\s*[:=]\s*"
            r"['\"]?(?!"
            r"$|REDACTED|CHANGEME|YOUR_|"
            r"os\.environ|window\.|self\.|request\.|data\.|"
            r"None|null|false|true|"
            r"[A-Z_][A-Z0-9_]*\b|"
            r"_?[A-Za-z_][A-Za-z0-9_]*\()"
            r"[A-Za-z0-9_./+=:-]{12,}"
        ),
    ),
    ("operator_name", re.compile(r"\b(?:Peter\s+Boose|LeMajesticMoose|MooseNet operator)\b", re.I)),
]


ALLOWED_MATCHES: set[tuple[str, str, int]] = {
    ("operator_name", "fleet/system/landing-page.html", 634),
}


@dataclass
class Finding:
    kind: str
    path: str
    line: int
    sample: str


def run_git(args: list[str], *, input_text: str | None = None, check: bool = True) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        input=input_text,
        text=True,
        capture_output=True,
        check=False,
    )
    if check and proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip())
    return proc.stdout


def should_skip(path: str) -> bool:
    p = Path(path)
    return path in SKIP_PATHS or p.name in SKIP_NAMES or p.suffix.lower() in SKIP_SUFFIXES or ".git/" in path


def clean_allowed(text: str) -> str:
    cleaned = text
    for _, pattern in ALLOWLIST:
        cleaned = pattern.sub("", cleaned)
    return cleaned


def iter_tracked_files() -> list[str]:
    return [p for p in run_git(["ls-files"]).splitlines() if p and not should_skip(p)]


def iter_staged_files() -> list[str]:
    out = run_git(["diff", "--cached", "--name-only", "--diff-filter=ACMRT"])
    return [p for p in out.splitlines() if p and not should_skip(p)]


def iter_range_files(rev_range: str) -> list[str]:
    out = run_git(["diff", "--name-only", "--diff-filter=ACMRT", rev_range])
    return [p for p in out.splitlines() if p and not should_skip(p)]


def read_worktree(path: str) -> str | None:
    try:
        return (ROOT / path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


def read_staged(path: str) -> str | None:
    proc = subprocess.run(
        ["git", "show", f":{path}"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        return None
    return proc.stdout


def read_revision(path: str, rev: str) -> str | None:
    proc = subprocess.run(
        ["git", "show", f"{rev}:{path}"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        return None
    return proc.stdout


def scan_text(path: str, text: str) -> list[Finding]:
    findings: list[Finding] = []
    for idx, raw_line in enumerate(text.splitlines(), start=1):
        line = clean_allowed(raw_line)
        for kind, pattern in PATTERNS:
            for match in pattern.finditer(line):
                if kind == "us_phone" and "lstrip(" in raw_line:
                    continue
                if (kind, path, idx) in ALLOWED_MATCHES:
                    continue
                sample = raw_line.strip()
                if len(sample) > MAX_SAMPLE:
                    sample = sample[: MAX_SAMPLE - 3] + "..."
                findings.append(Finding(kind, path, idx, sample))
                if len(findings) >= MAX_FINDINGS:
                    return findings
    return findings


def scan_files(paths: list[str], reader) -> list[Finding]:
    findings: list[Finding] = []
    for path in sorted(set(paths)):
        text = reader(path)
        if text is None:
            continue
        findings.extend(scan_text(path, text))
        if len(findings) >= MAX_FINDINGS:
            break
    return findings


def scan_push(stdin_text: str) -> list[Finding]:
    findings: list[Finding] = []
    for line in stdin_text.splitlines():
        parts = line.split()
        if len(parts) < 4:
            continue
        _local_ref, local_sha, _remote_ref, remote_sha = parts[:4]
        if re.fullmatch(r"0{40}", local_sha):
            continue
        if re.fullmatch(r"0{40}", remote_sha):
            paths = iter_range_files(local_sha)
        else:
            paths = iter_range_files(f"{remote_sha}..{local_sha}")
        findings.extend(scan_files(paths, lambda p, rev=local_sha: read_revision(p, rev)))
        if len(findings) >= MAX_FINDINGS:
            break
    return findings


def print_findings(findings: list[Finding]) -> None:
    print("")
    print("BLOCKED: privacy scan found sensitive content")
    print("")
    for finding in findings[:MAX_FINDINGS]:
        print(f"{finding.path}:{finding.line}: {finding.kind}: {finding.sample}")
    if len(findings) >= MAX_FINDINGS:
        print(f"... stopped after {MAX_FINDINGS} findings")
    print("")
    print("Remove or redact the content before committing or pushing to Gitea.")


def main() -> int:
    parser = argparse.ArgumentParser(description="Scan repo content for PII/secrets/private infrastructure details.")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--all", action="store_true", help="Scan all tracked files in the worktree.")
    mode.add_argument("--staged", action="store_true", help="Scan staged file contents.")
    mode.add_argument("--range", metavar="REV_RANGE", help="Scan files changed in a git revision range.")
    mode.add_argument("--pre-push", action="store_true", help="Read pre-push refs from stdin and scan pushed contents.")
    args = parser.parse_args()

    os.chdir(ROOT)

    if args.all:
        findings = scan_files(iter_tracked_files(), read_worktree)
    elif args.staged:
        findings = scan_files(iter_staged_files(), read_staged)
    elif args.range:
        end_rev = args.range.split("..")[-1]
        findings = scan_files(iter_range_files(args.range), lambda p: read_revision(p, end_rev))
    else:
        findings = scan_push(sys.stdin.read())

    if findings:
        print_findings(findings)
        return 1

    print("Privacy scan passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
