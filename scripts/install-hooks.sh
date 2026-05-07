#!/usr/bin/env bash
# Activate Dimmy's committed git hooks. Run this once per clone.
#
#   ./scripts/install-hooks.sh
#
# Sets `core.hooksPath` so git uses scripts/git-hooks/ (which IS
# committed to the repo, so updates propagate automatically) instead
# of .git/hooks/ (per-clone, not committed).
#
# To uninstall: `git config --unset core.hooksPath`

set -e

REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOKS_DIR="$REPO_ROOT/scripts/git-hooks"

if [ ! -d "$HOOKS_DIR" ]; then
    echo "✗ Hooks dir not found at $HOOKS_DIR"
    exit 1
fi

# Make the hooks executable (Windows clones lose the +x bit).
chmod +x "$HOOKS_DIR"/* 2>/dev/null || true

git config core.hooksPath scripts/git-hooks

echo "✓ Git hooks activated:"
ls -1 "$HOOKS_DIR" | sed 's/^/    /'
echo ""
echo "  pre-commit will run \`cargo fmt --check\` on staged Rust files"
echo "  before each commit. Skip ad-hoc with --no-verify."
