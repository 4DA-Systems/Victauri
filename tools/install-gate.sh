#!/bin/bash
# Install the the gate local pre-push gate for THIS repo. Idempotent; run once per clone.
# Sets core.hooksPath=.githooks (applies to the repo AND all its git worktrees — they share .git/config).
# The hook + gate spec are tracked (.githooks/pre-push, .gate/gate.json), so every worktree on a commit that
# has them will gate. Uninstall: git config --unset core.hooksPath.
set -e
ROOT="$(git rev-parse --show-toplevel)"
git -C "$ROOT" config core.hooksPath .githooks
echo "[local-gate] installed: core.hooksPath=.githooks (this repo + all worktrees)."
echo "  spec:    .gate/gate.json"
echo "  bypass:  git push --no-verify   (or LOCAL_GATE_SKIP=1)"
echo "  remove:  git -C \"$ROOT\" config --unset core.hooksPath"
