#!/bin/bash
# Install the Verax local pre-push gate for THIS repo. Idempotent; run once per clone.
# Sets core.hooksPath=.githooks (applies to the repo AND all its git worktrees — they share .git/config).
# The hook + gate spec are tracked (.githooks/pre-push, .verax/gate.json), so every worktree on a commit that
# has them will gate. Uninstall: git config --unset core.hooksPath.
set -e
ROOT="$(git rev-parse --show-toplevel)"
git -C "$ROOT" config core.hooksPath .githooks
echo "[verax-gate] installed: core.hooksPath=.githooks (this repo + all worktrees)."
echo "  spec:    .verax/gate.json"
echo "  bypass:  git push --no-verify   (or VERAX_SKIP_GATE=1)"
echo "  remove:  git -C \"$ROOT\" config --unset core.hooksPath"
