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
#   `.githooks/pre-push`, `.verax/gate.json`, and the gate installer/test helpers are TRACKED, so their content is
#   branch-controlled, and the gate executes `.verax/gate.json`'s `cmd` fields via `verax pipeline`. A bare delegate
#   would let a checked-out malicious branch run arbitrary commands on the developer's box the next time they
#   `git push` (contributor-workflow RCE). To close that: at install time we record a hash of the gate tooling under
#   `.git/` (UNtracked, so a branch cannot alter it), and the installed hook REFUSES to run (fail-closed) if any
#   tracked gate file no longer matches its pin. Legitimate gate changes surface as a loud "re-run
#   tools/install-gate.sh to re-pin" prompt after you have reviewed the diff — an attacker's silent swap cannot
#   execute.
set -e
ROOT="$(git rev-parse --show-toplevel)"
GATE_REL=".githooks/pre-push"
SPEC_REL=".verax/gate.json"
INSTALL_REL="tools/install-gate.sh"
TEST_REL="tools/test-install-gate.sh"
[ -f "$ROOT/$GATE_REL" ] || { echo "[verax-gate] no $GATE_REL in this repo — nothing to install"; exit 1; }

_vg_clean_path() {
  printf '%s' "$1" | tr -d '\r' | tr '\\' '/'
}

_vg_shell_path() {
  local p drive rest
  p="$(_vg_clean_path "$1")"
  case "$p" in
    [A-Za-z]:/*)
      if command -v cygpath >/dev/null 2>&1; then cygpath -u "$p"; return; fi
      drive="$(printf '%s' "$p" | cut -c1 | tr 'ABCDEFGHIJKLMNOPQRSTUVWXYZ' 'abcdefghijklmnopqrstuvwxyz')"
      rest="$(printf '%s' "${p#?:}" | sed 's#^/*##')"
      if [ -d "/mnt/$drive" ]; then printf '/mnt/%s/%s\n' "$drive" "$rest"; else printf '%s\n' "$p"; fi
      ;;
    *)
      printf '%s\n' "$p"
      ;;
  esac
}

_vg_config_hookdir() {
  local cfg p
  cfg="$1"
  if [ -n "$cfg" ]; then
    p="$(_vg_shell_path "$cfg")"
    case "$p" in /*|[A-Za-z]:/*) printf '%s\n' "$p" ;; *) printf '%s/%s\n' "$ROOT" "$p" ;; esac
  else
    printf '%s/.git/hooks\n' "$ROOT"
  fi
}

_vg_lower() {
  printf '%s' "$1" | tr 'ABCDEFGHIJKLMNOPQRSTUVWXYZ' 'abcdefghijklmnopqrstuvwxyz'
}

_vg_case_insensitive_paths() {
  local configured probe_dir probe_name probe_upper
  if [ -n "${VG_CASE_INSENSITIVE:-}" ]; then
    [ "$VG_CASE_INSENSITIVE" = 1 ] && return 0 || return 1
  fi

  configured="$(git -C "$ROOT" config --bool core.ignorecase 2>/dev/null || true)"
  [ "$configured" = "true" ] && { VG_CASE_INSENSITIVE=1; return 0; }

  probe_dir="$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || printf '%s/.git' "$ROOT")"
  probe_dir="$(_vg_clean_path "$probe_dir")"
  probe_name="vg-case-probe-$$"
  probe_upper="$(printf '%s' "$probe_name" | tr 'abcdefghijklmnopqrstuvwxyz' 'ABCDEFGHIJKLMNOPQRSTUVWXYZ')"
  rm -f "$probe_dir/$probe_name" "$probe_dir/$probe_upper" 2>/dev/null || true
  # FAIL-CLOSED on uncertainty: if we cannot write the probe we cannot RULE OUT a case-insensitive FS,
  # so assume insensitive (run the icase tracked-hook check) rather than assume sensitive and DROP it —
  # otherwise a probe-write failure on a case-insensitive FS silently reopens the R3-01 case-fold bypass.
  : > "$probe_dir/$probe_name" 2>/dev/null || { VG_CASE_INSENSITIVE=1; return 0; }
  if [ -e "$probe_dir/$probe_upper" ]; then
    rm -f "$probe_dir/$probe_name" "$probe_dir/$probe_upper" 2>/dev/null || true
    VG_CASE_INSENSITIVE=1
    return 0
  fi
  rm -f "$probe_dir/$probe_name" "$probe_dir/$probe_upper" 2>/dev/null || true
  VG_CASE_INSENSITIVE=0
  return 1
}

_vg_rel_under_root() {
  local path root path_l root_l root_len
  path="$(_vg_clean_path "$1")"
  root="$(_vg_clean_path "$ROOT")"
  case "$path" in
    "$root"/*) printf '%s\n' "${path#"$root"/}"; return 0 ;;
  esac
  if _vg_case_insensitive_paths; then
    path_l="$(_vg_lower "$path")"
    root_l="$(_vg_lower "$root")"
    case "$path_l" in
      "$root_l"/*)
        root_len=${#root}
        printf '%s\n' "${path:$((root_len + 1))}"
        return 0
        ;;
    esac
  fi
  return 1
}

_vg_tracked_path() {
  local rel="$1"
  git -C "$ROOT" ls-files --error-unmatch -- ":(literal)$rel" >/dev/null 2>&1 && return 0
  _vg_case_insensitive_paths && git -C "$ROOT" ls-files --error-unmatch -- ":(icase,literal)$rel" >/dev/null 2>&1 && return 0
  return 1
}

_vg_resolve_hookdir() {
  local cfg raw
  cfg="$1"
  raw="$(git rev-parse --path-format=absolute --git-path hooks 2>/dev/null || true)"
  raw="$(_vg_clean_path "$raw")"
  if [ -n "$raw" ]; then
    case "$raw" in
      *[A-Za-z]:/*)
        # Linux/WSL git treats a Windows-native core.hooksPath (D:\...) as repo-relative.
        # Convert the config value directly instead of accepting a repo-prefixed nonsense path.
        [ -n "$cfg" ] && { _vg_config_hookdir "$cfg"; return; }
        ;;
    esac
    printf '%s\n' "$raw"
    return
  fi
  _vg_config_hookdir "$cfg"
}

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

# Resolve the ACTIVE hook directory the way GIT ITSELF does — respecting core.hooksPath — as an
# absolute, FORWARD-SLASH path. We must write exactly where git reads: hand-parsing a Windows-native
# `core.hooksPath` (backslashes) is not portable across bash-on-Windows variants (Git Bash vs WSL vs
# pathconv-disabled) and could land the hardened hook somewhere git never reads while the OLD vulnerable
# hook stays active — a false "installed" that leaves the RCE open (GPT-5.5 audit 2e-02). `git rev-parse
# --git-path hooks` honors core.hooksPath; `--path-format=absolute` normalizes slashes for the shell.
cur="$(git config --get core.hooksPath || true)"
HOOKDIR="$(_vg_resolve_hookdir "$cur")"
[ -n "$cur" ] && KIND="custom ($cur)" || KIND="default"
mkdir -p "$HOOKDIR"
HOOK="$HOOKDIR/pre-push"
MARK="# >>> verax local gate >>>"
END="# <<< verax local gate <<<"

HOOK_REL="$(_vg_rel_under_root "$HOOK" || true)"
HOOKDIR_PHYS="$(cd "$HOOKDIR" 2>/dev/null && pwd -P || true)"
HOOK_PHYS_REL=""
[ -n "$HOOKDIR_PHYS" ] && HOOK_PHYS_REL="$(_vg_rel_under_root "$HOOKDIR_PHYS/pre-push" || true)"
for __vg_hook_rel in "$HOOK_REL" "$HOOK_PHYS_REL"; do
  [ -n "$__vg_hook_rel" ] || continue
  if _vg_tracked_path "$__vg_hook_rel"; then
    echo "[verax-gate] ERROR: active pre-push hook is tracked by this repo: $__vg_hook_rel"
    echo "[verax-gate]   Refusing to install the verifier into branch-controlled hook content."
    echo "[verax-gate]   Set core.hooksPath to an untracked hook directory (for example .git/hooks or .husky/_), then re-run this script."
    exit 1
  fi
done

# Write the integrity pins to an UNtracked path inside the COMMON git dir — the same dir the shared hooks
# live in, so one install covers every linked worktree (a per-worktree `--git-path` would not be found by a
# push from a different worktree, silently fail-closing it).
PIN="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || git rev-parse --git-common-dir)/verax-gate.pin"
{
  echo "pre-push $(_vg_hash "$ROOT/$GATE_REL")"
  # Pin gate.json UNCONDITIONALLY — even when absent (_vg_hash prints the "absent" sentinel). If we skipped
  # the line for a missing file, the delegate's verify loop (which checks only pinned lines) would not cover
  # gate.json, and a branch that ADDS a gate.json after install would run un-pinned via .githooks/pre-push's
  # `verax pipeline --spec .verax/gate.json`. Pinning "absent" makes an add-after-install read as drift
  # (real hash != "absent") → the fail-closed refuse fires. A repo that legitimately never has gate.json
  # stays green (absent == absent).
  echo "gate.json $(_vg_hash "$ROOT/$SPEC_REL")"
  echo "install-gate.sh $(_vg_hash "$ROOT/$INSTALL_REL")"
  echo "test-install-gate.sh $(_vg_hash "$ROOT/$TEST_REL")"
} > "$PIN"
echo "[verax-gate] pinned gate integrity → $PIN"

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
    install-gate.sh) __vg_file="$__vg_root/tools/install-gate.sh" ;;
    test-install-gate.sh) __vg_file="$__vg_root/tools/test-install-gate.sh" ;;
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
  ACTION="refreshed the gate block in"
elif [ -f "$HOOK" ]; then
  # CHAIN: keep the existing pre-push, append our verified-delegate block.
  { echo ""; echo "$MARK"; echo "$DELEGATE"; echo "$END"; } >> "$HOOK"
  ACTION="CHAINED the gate onto the existing"
else
  # No existing pre-push: create one. Husky sources its hooks, so it needs no shebang; others do.
  case "$HOOKDIR" in *.husky*) : ;; *) printf '#!/bin/bash\n' > "$HOOK" ;; esac
  { echo "$MARK"; echo "$DELEGATE"; echo "$END"; } >> "$HOOK"
  ACTION="installed a new"
fi
chmod +x "$HOOK" 2>/dev/null || true

# POST-INSTALL ASSERTION: confirm the gate is present at the hook path GIT WILL ACTUALLY USE. Because
# HOOKDIR is git's own resolved hooks path, this holds by construction — but we verify rather than assume,
# so a path-resolution quirk can never leave a false "installed" while the OLD (ungated) hook stays active.
ACTIVE="$(_vg_resolve_hookdir "$(git config --get core.hooksPath || true)")/pre-push"
if ! grep -qF "$MARK" "$ACTIVE" 2>/dev/null; then
  echo "[verax-gate] ERROR: post-install check FAILED — the gate is NOT at the active hook path git will run:"
  echo "[verax-gate]   $ACTIVE"
  echo "[verax-gate]   Your pushes are NOT gated. Refusing to report success. (core.hooksPath=${cur:-<unset>})"
  exit 1
fi
echo "[verax-gate] $ACTION $KIND hook ($HOOK)."
echo "  gate spec: .verax/gate.json (integrity-pinned)   ·   bypass: git push --no-verify (or VERAX_SKIP_GATE=1)"
echo "  remove:    delete the '$MARK ... $END' block from $HOOK (and $PIN)"
