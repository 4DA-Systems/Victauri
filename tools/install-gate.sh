#!/bin/bash
# Install the Verax local pre-push gate — WITHOUT clobbering an existing hook system.
#
# The gate LOGIC lives in the tracked `.githooks/pre-push`. This script wires a thin DELEGATING pre-push hook
# into whatever hook directory the repo ALREADY uses (Husky `.husky/`, a custom `core.hooksPath`, or the default
# `.git/hooks`), and CHAINS any pre-push already there. It never changes which directory is active, so existing
# hooks (pre-commit lint/fmt, Husky's CADE checks, etc.) keep working. Idempotent. Uninstall: remove the
# "verax local gate" block from the active pre-push (printed below).
#
# SECURITY — integrity pinning (why the delegate is not a bare `bash .githooks/pre-push`):
#   `.githooks/pre-push` and `.verax/gate.json` are TRACKED, so their content is branch-controlled, and the gate
#   executes `.verax/gate.json`'s `cmd` fields via `verax pipeline`. A bare delegate would let a checked-out
#   malicious branch run arbitrary commands on the developer's box the next time they `git push` (contributor-
#   workflow RCE). To close that: at install time we record a hash of both files under `.git/` (UNtracked, so a
#   branch cannot alter it), and the installed hook REFUSES to run (fail-closed) if either tracked file no longer
#   matches its pin. Legitimate gate changes surface as a loud "re-run tools/install-gate.sh to re-pin" prompt
#   after you have reviewed the diff — an attacker's silent swap cannot execute.
set -e
ROOT="$(git rev-parse --show-toplevel)"
GATE_REL=".githooks/pre-push"
SPEC_REL=".verax/gate.json"
[ -f "$ROOT/$GATE_REL" ] || { echo "[verax-gate] no $GATE_REL in this repo — nothing to install"; exit 1; }

# Portable content hash: prefer sha256, fall back to git's blob hash (always present in a git hook).
# Prints "<algo>:<hex>" so the installed hook verifies with the SAME algorithm. Never prints an empty hash.
_vg_hash() {
  local f="$1" h=""
  if [ ! -f "$f" ]; then echo "absent"; return; fi
  if command -v sha256sum >/dev/null 2>&1; then h="$(sha256sum "$f" 2>/dev/null | cut -d' ' -f1)"; [ -n "$h" ] && { echo "sha256:$h"; return; }; fi
  if command -v shasum   >/dev/null 2>&1; then h="$(shasum -a 256 "$f" 2>/dev/null | cut -d' ' -f1)"; [ -n "$h" ] && { echo "sha256:$h"; return; }; fi
  if command -v openssl  >/dev/null 2>&1; then h="$(openssl dgst -sha256 "$f" 2>/dev/null | awk '{print $NF}')"; [ -n "$h" ] && { echo "sha256:$h"; return; }; fi
  h="$(git hash-object "$f" 2>/dev/null)"; [ -n "$h" ] && { echo "githash:$h"; return; }
  echo "unhashable"
}

# Write the integrity pins to an UNtracked path inside the COMMON git dir — the same dir the shared hooks
# live in, so one install covers every linked worktree (a per-worktree `--git-path` would not be found by a
# push from a different worktree, silently fail-closing it).
PIN="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || git rev-parse --git-common-dir)/verax-gate.pin"
{
  echo "pre-push $(_vg_hash "$ROOT/$GATE_REL")"
  [ -f "$ROOT/$SPEC_REL" ] && echo "gate.json $(_vg_hash "$ROOT/$SPEC_REL")"
} > "$PIN"
echo "[verax-gate] pinned gate integrity → $PIN"

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

# The verified delegate: check the tracked gate files against the install-time pins BEFORE running them.
# This block is written into the UNtracked hook, so a branch cannot remove or weaken the check. Fail-closed:
# a missing pin, an unhashable file, or any drift refuses the push instead of executing branch content.
read -r -d '' DELEGATE <<'DELEGATE_EOF' || true
__vg_root="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
__vg_pin="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || git rev-parse --git-common-dir)/verax-gate.pin"
__vg_hash() {
  local f="$1" h=""
  if [ ! -f "$f" ]; then echo "absent"; return; fi
  if command -v sha256sum >/dev/null 2>&1; then h="$(sha256sum "$f" 2>/dev/null | cut -d' ' -f1)"; [ -n "$h" ] && { echo "sha256:$h"; return; }; fi
  if command -v shasum   >/dev/null 2>&1; then h="$(shasum -a 256 "$f" 2>/dev/null | cut -d' ' -f1)"; [ -n "$h" ] && { echo "sha256:$h"; return; }; fi
  if command -v openssl  >/dev/null 2>&1; then h="$(openssl dgst -sha256 "$f" 2>/dev/null | awk '{print $NF}')"; [ -n "$h" ] && { echo "sha256:$h"; return; }; fi
  h="$(git hash-object "$f" 2>/dev/null)"; [ -n "$h" ] && { echo "githash:$h"; return; }
  echo "unhashable"
}
if [ -z "$__vg_pin" ] || [ ! -f "$__vg_pin" ]; then
  echo "[verax-gate] REFUSING: no integrity pin found — will not run branch-controlled gate content."
  echo "[verax-gate]   Re-run tools/install-gate.sh (from a trusted checkout) to pin the gate, or bypass: git push --no-verify"
  exit 1
fi
__vg_fail=0
while read -r __vg_name __vg_expect; do
  case "$__vg_name" in
    pre-push)  __vg_file="$__vg_root/.githooks/pre-push" ;;
    gate.json) __vg_file="$__vg_root/.verax/gate.json" ;;
    *) continue ;;
  esac
  __vg_got="$(__vg_hash "$__vg_file")"
  if [ "$__vg_got" != "$__vg_expect" ]; then
    echo "[verax-gate] REFUSING: $__vg_name changed since install (pinned=$__vg_expect got=$__vg_got)."
    echo "[verax-gate]   A checked-out branch may have altered the gate. Review the diff, then re-run"
    echo "[verax-gate]   tools/install-gate.sh to re-pin — or bypass this push with: git push --no-verify"
    __vg_fail=1
  fi
done < "$__vg_pin"
[ "$__vg_fail" = 0 ] || exit 1
bash "$__vg_root/.githooks/pre-push" "$@" || exit 1
DELEGATE_EOF

if [ -f "$HOOK" ] && grep -qF "$MARK" "$HOOK"; then
  # Re-pin (done above) but refresh the block too, so upgrades to this installer take effect.
  tmp="$(mktemp)"; awk -v m="$MARK" -v e="$END" '$0==m{s=1} !s{print} $0==e{s=0}' "$HOOK" > "$tmp" && mv "$tmp" "$HOOK"
  { echo ""; echo "$MARK"; echo "$DELEGATE"; echo "$END"; } >> "$HOOK"
  chmod +x "$HOOK" 2>/dev/null || true
  echo "[verax-gate] refreshed the gate block in the $KIND hook ($HOOK)."
  echo "  gate spec: .verax/gate.json (integrity-pinned)   ·   bypass: git push --no-verify (or VERAX_SKIP_GATE=1)"
  exit 0
fi

if [ -f "$HOOK" ]; then
  # CHAIN: keep the existing pre-push, append our verified-delegate block.
  { echo ""; echo "$MARK"; echo "$DELEGATE"; echo "$END"; } >> "$HOOK"
  echo "[verax-gate] CHAINED the gate onto the existing $KIND pre-push ($HOOK)."
else
  # No existing pre-push: create one (with a shebang for non-husky dirs).
  { [ "$KIND" = husky ] || echo "#!/bin/bash"; echo "$MARK"; echo "$DELEGATE"; echo "$END"; } > "$HOOK"
  echo "[verax-gate] installed a new $KIND pre-push ($HOOK)."
fi
chmod +x "$HOOK" 2>/dev/null || true
echo "  gate spec: .verax/gate.json (integrity-pinned)   ·   bypass: git push --no-verify (or VERAX_SKIP_GATE=1)"
echo "  remove:    delete the '$MARK ... $END' block from $HOOK (and $PIN)"
