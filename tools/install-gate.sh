#!/bin/bash
# Install the Verax local pre-push gate — WITHOUT clobbering an existing hook system.
#
# The gate LOGIC lives in the tracked `.githooks/pre-push`. This script wires a thin DELEGATING pre-push hook
# into whatever hook directory the repo ALREADY uses (Husky `.husky/`, a custom `core.hooksPath`, or the default
# `.git/hooks`), and CHAINS any pre-push already there. It never changes which directory is active, so existing
# hooks (pre-commit lint/fmt, Husky's CADE checks, etc.) keep working. Idempotent. Uninstall: remove the
# "verax local gate" block from the active pre-push (printed below).
set -e
ROOT="$(git rev-parse --show-toplevel)"
GATE_REL=".githooks/pre-push"
[ -f "$ROOT/$GATE_REL" ] || { echo "[verax-gate] no $GATE_REL in this repo — nothing to install"; exit 1; }

# Pick the ACTIVE hook directory (do NOT change it).
cur="$(git config --get core.hooksPath || true)"
if [ -d "$ROOT/.husky" ]; then
  HOOKDIR="$ROOT/.husky"; KIND="husky"
elif [ -n "$cur" ]; then
  case "$cur" in /*|[A-Za-z]:*) HOOKDIR="$cur" ;; *) HOOKDIR="$ROOT/$cur" ;; esac  # relative → repo-rooted
  KIND="custom ($cur)"
else
  HOOKDIR="$ROOT/.git/hooks"; KIND="default"
fi
mkdir -p "$HOOKDIR"
HOOK="$HOOKDIR/pre-push"
MARK="# >>> verax local gate >>>"
END="# <<< verax local gate <<<"
DELEGATE="bash \"\$(git rev-parse --show-toplevel)/$GATE_REL\" \"\$@\" || exit 1"

if [ -f "$HOOK" ] && grep -qF "$MARK" "$HOOK"; then
  echo "[verax-gate] already installed in $KIND hook ($HOOK)."
  exit 0
fi

if [ -f "$HOOK" ]; then
  # CHAIN: keep the existing pre-push, append our delegating block.
  { echo ""; echo "$MARK"; echo "$DELEGATE"; echo "$END"; } >> "$HOOK"
  echo "[verax-gate] CHAINED the gate onto the existing $KIND pre-push ($HOOK)."
else
  # No existing pre-push: create one (with a shebang for non-husky dirs).
  { [ "$KIND" = husky ] || echo "#!/bin/bash"; echo "$MARK"; echo "$DELEGATE"; echo "$END"; } > "$HOOK"
  echo "[verax-gate] installed a new $KIND pre-push ($HOOK)."
fi
chmod +x "$HOOK" 2>/dev/null || true
echo "  gate spec: .verax/gate.json   ·   bypass: git push --no-verify (or VERAX_SKIP_GATE=1)"
echo "  remove:    delete the '$MARK ... $END' block from $HOOK"
