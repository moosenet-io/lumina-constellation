#!/bin/bash
# publish-to-github.sh — Publish from Gitea (dev) to GitHub (prod)
# Run PII scan, show diff, confirm, push.
# Usage: ./deploy/publish-to-github.sh

set -e

PATTERNS='192\.168\.[0-9]+\.[0-9]+|[a-zA-Z0-9._%+-]+@(gmail|yahoo|outlook)\.(com|net)|PASSWORD\s*=\s*\S+|API_KEY\s*=\s*\S+|sk-[a-zA-Z0-9]{20,}|SECRET\s*=\s*\S+|\+1[0-9]{10}'

echo "=== Lumina Constellation: Publish to GitHub ==="
echo ""

# Fetch both remotes
echo "Fetching gitea and github..."
git fetch gitea --quiet
git fetch github 2>/dev/null || true

# Check how far ahead gitea is
COMMIT_COUNT=$(git log --oneline github/main..gitea/main 2>/dev/null | wc -l | tr -d ' ')

if [ "$COMMIT_COUNT" -eq 0 ]; then
    echo "Nothing to publish. Gitea and GitHub are already in sync."
    exit 0
fi

echo "Found $COMMIT_COUNT commit(s) to publish:"
git log --oneline github/main..gitea/main
echo ""

# PII scan
echo "=== Running PII scan ==="
FOUND=0
if git log -p github/main..gitea/main 2>/dev/null | grep -E "^\+" | grep -vE "^\+\+\+" | grep -qE "$PATTERNS" 2>/dev/null; then
    FOUND=1
fi

if [ "$FOUND" -gt 0 ]; then
    echo ""
    echo "╔══════════════════════════════════════════════════════════╗"
    echo "║  PII DETECTED — BLOCKING PUBLISH                        ║"
    echo "╚══════════════════════════════════════════════════════════╝"
    echo ""
    git log -p github/main..gitea/main | grep -E "^\+" | grep -vE "^\+\+\+" | grep -nE "$PATTERNS" | head -30
    echo ""
    echo "Fix on Gitea first (amend/rebase commits), then run this script again."
    exit 1
fi

echo "PII scan PASSED."
echo ""

# Confirm
read -r -p "Publish $COMMIT_COUNT commit(s) to github.com/moosenet-io/lumina-constellation? [y/N] " confirm
if [ "$confirm" != "y" ] && [ "$confirm" != "Y" ]; then
    echo "Aborted."
    exit 0
fi

# Enable push to GitHub temporarily
GITHUB_TOKEN=$(grep GITHUB_TOKEN /home/coder/.env 2>/dev/null | cut -d= -f2 || echo "")
if [ -z "$GITHUB_TOKEN" ]; then
    echo "Error: GITHUB_TOKEN not found in /home/coder/.env"
    exit 1
fi

git remote set-url --push github "https://${GITHUB_TOKEN}@github.com/moosenet-io/lumina-constellation.git"
git push github gitea/main:main
# Disable push again
git remote set-url --push github DISABLED

echo ""
echo "Published successfully to GitHub."
echo "https://github.com/moosenet-io/lumina-constellation"
