#!/bin/bash
# Install the the gate local pre-push gate — WITHOUT clobbering an existing hook system.
#
# The gate LOGIC lives in the tracked `.githooks/pre-push`. This script wires a thin DELEGATING pre-push hook
# into whatever hook directory the repo ALREADY uses (Husky `.husky/`, a custom `core.hooksPath`, or the default
# `.git/hooks`), and CHAINS any pre-push already there. It never changes which directory is active, so existing
# hooks (pre-commit lint/fmt, Husky's CADE checks, etc.) keep working. Idempotent. Uninstall: remove the
# "gate local gate" block from the active pre-push (printed below).
#
# SECURITY — integrity pinning (why the delegate is not a bare `bash .githooks/pre-push`):
#   `.githooks/pre-push`, `.gate/gate.json`, and the gate installer/test helpers are TRACKED, so their content is
#   branch-controlled, and the gate executes `.gate/gate.json`'s `cmd` fields via `gate-runner pipeline`. A bare delegate
#   would let a checked-out malicious branch run arbitrary commands on the developer's box the next time they
#   `git push` (contributor-workflow RCE). To close that: at install time we record a hash of the gate tooling under
#   `.git/` (UNtracked, so a branch cannot alter it), and the installed hook REFUSES to run (fail-closed) if any
#   tracked gate file no longer matches its pin. Legitimate gate changes surface as a loud "re-run
#   tools/install-gate.sh to re-pin" prompt after you have reviewed the diff — an attacker's silent swap cannot
#   execute.
#   SCOPE (do not overclaim): the pin closes the gate-SCRIPT-tampering vector (a branch making `git push`
#   run its OWN command with no build). It does NOT — and cannot — make it safe to push a HOSTILE branch,
#   because the gate's own trusted job runs `cargo clippy`/`cargo test` on the working tree, which COMPILES
#   it (build.rs + proc-macros + test code = arbitrary execution). This gate protects a developer pushing
#   code they authored/trust; it is not a sandbox for pushing a branch you would not already build/run.
set -e
ROOT="$(git rev-parse --show-toplevel)"
GATE_REL=".githooks/pre-push"
SPEC_REL=".gate/gate.json"
INSTALL_REL="tools/install-gate.sh"
TEST_REL="tools/test-install-gate.sh"
[ -f "$ROOT/$GATE_REL" ] || { echo "[local-gate] no $GATE_REL in this repo — nothing to install"; exit 1; }

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
  local up
  if [ -n "${VG_CASE_INSENSITIVE:-}" ]; then
    [ "$VG_CASE_INSENSITIVE" = 1 ] && return 0 || return 1
  fi

  # Decide case-folding on the WORKTREE filesystem that hosts the protected tracked hook — NOT on
  # `--git-common-dir`. A LINKED worktree can place `.githooks` on a case-INsensitive volume while the git
  # common dir lives on a case-SENSITIVE one; probing the common dir then mis-detects "case-sensitive",
  # drops the `:(icase)` tracked-hook check, and `.GITHOOKS` aliases the tracked `.githooks/pre-push`
  # (GPT-5.5 audit R4-01, repro: common dir on /tmp, worktree on /mnt/d). The tracked hook ($GATE_REL) is
  # GUARANTEED to exist here (checked at the top of the script), so whether an alternate-case spelling of it
  # ALSO resolves is a definitive, write-free test of THIS volume's case behavior — no probe, no wrong-FS.
  up="$(printf '%s' "$GATE_REL" | tr 'a-z' 'A-Z')"
  if [ -e "$ROOT/$GATE_REL" ] && [ "$up" != "$GATE_REL" ]; then
    if [ -e "$ROOT/$up" ]; then VG_CASE_INSENSITIVE=1; return 0; fi
    VG_CASE_INSENSITIVE=0; return 1
  fi

  # Defensive fallback (only reachable if the protected path is unexpectedly absent): git's own view, else
  # FAIL-CLOSED to insensitive (run the icase check) so uncertainty never DROPS protection.
  [ "$(git -C "$ROOT" config --bool core.ignorecase 2>/dev/null || true)" = "true" ] && { VG_CASE_INSENSITIVE=1; return 0; }
  VG_CASE_INSENSITIVE=1
  return 0
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
MARK="# >>> local gate >>>"
END="# <<< local gate <<<"

# Filesystem-IDENTITY tracked-hook check (PRIMARY) — immune to case/Unicode/symlink/path-spelling.
# String matching on the hook path repeatedly missed aliases (R2/R3/R4/R5). We use FILESYSTEM IDENTITY
# (`-ef` = device+inode) + git's own resolution: git + the filesystem resolve the REAL object no matter how
# `core.hooksPath` spelled it (case, Unicode, symlink, `./`, `//`). Branch-controlled = a tracked dir/file in
# THIS repo, a SUBMODULE of this repo, or a tracked GITLINK (GPT-5.5 audits R5-01, R6-01, R6-02). Refuse all.
_vg_refuse_hook() {
  echo "[local-gate] ERROR: $1"
  echo "[local-gate]   Refusing to install the verifier into branch-controlled hook content."
  echo "[local-gate]   Set core.hooksPath to an untracked hook directory (for example .git/hooks or .husky/_), then re-run this script."
  exit 1
}
if [ -d "$HOOKDIR" ]; then
  # (a) hook dir is inside THIS repo's working tree and git tracks content in it
  _vg_hd_top="$(git -C "$HOOKDIR" rev-parse --show-toplevel 2>/dev/null || true)"
  if [ -n "$_vg_hd_top" ] && [ "$_vg_hd_top" -ef "$ROOT" ] \
     && [ -n "$(git -C "$HOOKDIR" ls-files 2>/dev/null | head -n 1)" ]; then
    _vg_refuse_hook "active pre-push hook directory is tracked by this repo (branch-controlled)."
  fi
  # (b) hook dir is inside a SUBMODULE of this repo — its own --show-toplevel is the submodule (so (a) misses
  # it). Walk the whole superproject chain: a nested submodule reports its immediate parent first, not $ROOT,
  # but the root gitlink still branch-controls the reachable hook content.
  _vg_subdir="$HOOKDIR"
  _vg_depth=0
  while :; do
    _vg_super="$(git -C "$_vg_subdir" rev-parse --show-superproject-working-tree 2>/dev/null || true)"
    [ -n "$_vg_super" ] || break
    if [ "$_vg_super" -ef "$ROOT" ]; then
      _vg_refuse_hook "active pre-push hook directory is inside a submodule tracked by this repo (branch-controlled)."
    fi
    [ "$_vg_super" -ef "$_vg_subdir" ] && break
    _vg_subdir="$_vg_super"
    _vg_depth=$((_vg_depth + 1))
    [ "$_vg_depth" -le 32 ] || _vg_refuse_hook "active pre-push hook directory is inside an unusually deep submodule chain; refusing to avoid missing branch-controlled content."
  done
fi
# (c) hook dir IS a tracked gitlink (mode 160000) of this repo — a submodule that may not be checked out yet;
# a later `git submodule update` makes it branch-controlled, so fail closed now (R6-01). Do not trust
# `.gitmodules` as the enumerator: a branch can create a bare gitlink or omit the module entry. `--stage -z`
# gives raw (unquoted) paths.
while IFS= read -r -d '' _vg_rec; do
  case "$_vg_rec" in 160000\ *) ;; *) continue ;; esac
  _vg_gl="${_vg_rec#*$'\t'}"
  [ -n "$_vg_gl" ] && [ -e "$ROOT/$_vg_gl" ] || continue
  if [ "$HOOKDIR" -ef "$ROOT/$_vg_gl" ]; then
    _vg_refuse_hook "active pre-push hook directory is a tracked submodule (gitlink) of this repo (branch-controlled)."
  fi
done < <(git -C "$ROOT" ls-files --stage -z 2>/dev/null)
# (d) FILE identity: the active hook FILE resolves (by device+inode) to a tracked file — e.g. an untracked
# dir holding a `pre-push` symlinked to a tracked hook with any basename. `-z` yields RAW paths: git QUOTES
# non-ASCII paths by default (core.quotePath), which made `[ -e ]` miss the target (R6-02).
if [ -e "$HOOK" ]; then
  while IFS= read -r -d '' _vg_tf; do
    [ -n "$_vg_tf" ] && [ -e "$ROOT/$_vg_tf" ] || continue
    if [ "$HOOK" -ef "$ROOT/$_vg_tf" ]; then
      _vg_refuse_hook "active pre-push hook resolves to a tracked file ($_vg_tf) — tracked by this repo (branch-controlled)."
    fi
  done < <(git -C "$ROOT" ls-files -z 2>/dev/null)
fi

# Secondary net (index-only / not-yet-checked-out tracked hook, where the dir identity check above cannot
# see a filesystem object): the original literal + icase pathspec check on the string-resolved relative path.
HOOK_REL="$(_vg_rel_under_root "$HOOK" || true)"
HOOKDIR_PHYS="$(cd "$HOOKDIR" 2>/dev/null && pwd -P || true)"
HOOK_PHYS_REL=""
[ -n "$HOOKDIR_PHYS" ] && HOOK_PHYS_REL="$(_vg_rel_under_root "$HOOKDIR_PHYS/pre-push" || true)"
for __vg_hook_rel in "$HOOK_REL" "$HOOK_PHYS_REL"; do
  [ -n "$__vg_hook_rel" ] || continue
  if _vg_tracked_path "$__vg_hook_rel"; then
    echo "[local-gate] ERROR: active pre-push hook is tracked by this repo: $__vg_hook_rel"
    echo "[local-gate]   Refusing to install the verifier into branch-controlled hook content."
    echo "[local-gate]   Set core.hooksPath to an untracked hook directory (for example .git/hooks or .husky/_), then re-run this script."
    exit 1
  fi
done

# Write the integrity pins to an UNtracked path inside the COMMON git dir — the same dir the shared hooks
# live in, so one install covers every linked worktree (a per-worktree `--git-path` would not be found by a
# push from a different worktree, silently fail-closing it).
PIN="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || git rev-parse --git-common-dir)/local-gate.pin"
{
  echo "pre-push $(_vg_hash "$ROOT/$GATE_REL")"
  # Pin gate.json UNCONDITIONALLY — even when absent (_vg_hash prints the "absent" sentinel). If we skipped
  # the line for a missing file, the delegate's verify loop (which checks only pinned lines) would not cover
  # gate.json, and a branch that ADDS a gate.json after install would run un-pinned via .githooks/pre-push's
  # `gate-runner pipeline --spec .gate/gate.json`. Pinning "absent" makes an add-after-install read as drift
  # (real hash != "absent") → the fail-closed refuse fires. A repo that legitimately never has gate.json
  # stays green (absent == absent).
  echo "gate.json $(_vg_hash "$ROOT/$SPEC_REL")"
  echo "install-gate.sh $(_vg_hash "$ROOT/$INSTALL_REL")"
  echo "test-install-gate.sh $(_vg_hash "$ROOT/$TEST_REL")"
} > "$PIN"
echo "[local-gate] pinned gate integrity → $PIN"

# The verified delegate: check the tracked gate files against the install-time pins BEFORE running them.
# This block is written into the UNtracked hook, so a branch cannot remove or weaken the check. Fail-closed:
# a missing pin, an unhashable file, or any drift refuses the push instead of executing branch content.
read -r -d '' DELEGATE <<'DELEGATE_EOF' || true
__vg_root="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
__vg_pin="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || git rev-parse --git-common-dir)/local-gate.pin"
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
  echo "[local-gate] REFUSING: no integrity pin found — will not run branch-controlled gate content."
  echo "[local-gate]   Re-run tools/install-gate.sh (from a trusted checkout) to pin the gate, or bypass: git push --no-verify"
  exit 1
fi
__vg_fail=0
while read -r __vg_name __vg_expect; do
  case "$__vg_name" in
    pre-push)  __vg_file="$__vg_root/.githooks/pre-push" ;;
    gate.json) __vg_file="$__vg_root/.gate/gate.json" ;;
    install-gate.sh) __vg_file="$__vg_root/tools/install-gate.sh" ;;
    test-install-gate.sh) __vg_file="$__vg_root/tools/test-install-gate.sh" ;;
    *) continue ;;
  esac
  __vg_got="$(__vg_hash "$__vg_file")"
  if [ "$__vg_got" != "$__vg_expect" ]; then
    echo "[local-gate] REFUSING: $__vg_name changed since install (pinned=$__vg_expect got=$__vg_got)."
    echo "[local-gate]   A checked-out branch may have altered the gate. Review the diff, then re-run"
    echo "[local-gate]   tools/install-gate.sh to re-pin — or bypass this push with: git push --no-verify"
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
  echo "[local-gate] ERROR: post-install check FAILED — the gate is NOT at the active hook path git will run:"
  echo "[local-gate]   $ACTIVE"
  echo "[local-gate]   Your pushes are NOT gated. Refusing to report success. (core.hooksPath=${cur:-<unset>})"
  exit 1
fi
echo "[local-gate] $ACTION $KIND hook ($HOOK)."
echo "  gate spec: .gate/gate.json (integrity-pinned)   ·   bypass: git push --no-verify (or LOCAL_GATE_SKIP=1)"
echo "  remove:    delete the '$MARK ... $END' block from $HOOK (and $PIN)"
