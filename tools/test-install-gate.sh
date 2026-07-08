#!/bin/bash
# Regression tests for tools/install-gate.sh. These exercise the integrity pin
# against branch-controlled hook/gate drift and path-resolution edge cases.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"

case "$ROOT" in
  /mnt/[A-Za-z]/*)
    mkdir -p "$ROOT/target"
    BASE="$(mktemp -d "$ROOT/target/gate-install-tests.XXXXXX")"
    ;;
  *)
    BASE="$(mktemp -d)"
    ;;
esac
cleanup() {
  case "$BASE" in
    "$ROOT"/target/gate-install-tests.*|/tmp/*)
      rm -rf "$BASE"
      ;;
  esac
}
trap cleanup EXIT
mkdir -p "$BASE/logs"
printf '[install-gate-test] base=%s\n' "$BASE"

make_repo() {
  local dir="$1" with_spec="${2:-yes}"
  mkdir -p "$dir"
  cd "$dir"
  git init -q
  git config user.email audit@example.test
  git config user.name audit
  mkdir -p tools .githooks
  cp "$ROOT/tools/install-gate.sh" tools/install-gate.sh
  cp "$ROOT/tools/test-install-gate.sh" tools/test-install-gate.sh
  cp "$ROOT/.githooks/pre-push" .githooks/pre-push
  chmod +x tools/install-gate.sh tools/test-install-gate.sh .githooks/pre-push
  if [ "$with_spec" = yes ]; then
    mkdir -p .gate
    cp "$ROOT/.gate/gate.json" .gate/gate.json
  fi
  git add tools/install-gate.sh tools/test-install-gate.sh .githooks/pre-push
  [ "$with_spec" = yes ] && git add .gate/gate.json
  git commit -m init -q
}

assert_contains() {
  local file="$1" needle="$2"
  grep -qF "$needle" "$file" || { echo "[install-gate-test] missing '$needle' in $file"; return 1; }
}

# Default hook install must verify tracked pre-push drift before executing it.
make_repo "$BASE/default" yes
cd "$BASE/default"
bash tools/install-gate.sh > "$BASE/logs/default-install.out"
assert_contains .git/hooks/pre-push '# >>> local gate >>>'
printf '#!/bin/bash\necho PWNED > "$PWD/pwned"\nexit 0\n' > .githooks/pre-push
chmod +x .githooks/pre-push
if bash .git/hooks/pre-push > "$BASE/logs/default-hook.out" 2>&1; then
  echo '[install-gate-test] default drift unexpectedly passed'
  exit 10
fi
[ ! -f pwned ] || { echo '[install-gate-test] default drift executed malicious hook'; exit 11; }
assert_contains "$BASE/logs/default-hook.out" 'REFUSING: pre-push changed since install'
printf '[install-gate-test] default drift blocked\n'

# gate.json absent at install must be pinned absent; adding it later is drift.
make_repo "$BASE/absent" no
cd "$BASE/absent"
bash tools/install-gate.sh > "$BASE/logs/absent-install.out"
assert_contains .git/local-gate.pin 'gate.json absent'
mkdir -p .gate
printf '{"jobs":[{"id":"pwn","cmd":"echo PWNED > pwned"}]}\n' > .gate/gate.json
if bash .git/hooks/pre-push > "$BASE/logs/absent-hook.out" 2>&1; then
  echo '[install-gate-test] add-after-install unexpectedly passed'
  exit 12
fi
[ ! -f pwned ] || { echo '[install-gate-test] add-after-install executed gate'; exit 13; }
assert_contains "$BASE/logs/absent-hook.out" 'REFUSING: gate.json changed since install'
printf '[install-gate-test] add-after-install blocked\n'

# Gate helper drift must block before .githooks/pre-push can run branch-controlled helper code.
make_repo "$BASE/helper-drift" yes
cd "$BASE/helper-drift"
bash tools/install-gate.sh > "$BASE/logs/helper-install.out"
printf '#!/bin/bash\necho PWNED > "$PWD/pwned"\nexit 0\n' > tools/test-install-gate.sh
chmod +x tools/test-install-gate.sh
if bash .git/hooks/pre-push > "$BASE/logs/helper-hook.out" 2>&1; then
  echo '[install-gate-test] helper drift unexpectedly passed'
  exit 24
fi
[ ! -f pwned ] || { echo '[install-gate-test] helper drift executed malicious helper'; exit 25; }
assert_contains "$BASE/logs/helper-hook.out" 'REFUSING: test-install-gate.sh changed since install'
printf '[install-gate-test] helper drift blocked\n'

# A tracked active hook path cannot host the verifier; Git would execute branch-controlled content first.
make_repo "$BASE/tracked-hooks" yes
cd "$BASE/tracked-hooks"
git config core.hooksPath .githooks
if bash tools/install-gate.sh > "$BASE/logs/tracked-install.out" 2>&1; then
  echo '[install-gate-test] tracked hooksPath install unexpectedly passed'
  exit 14
fi
assert_contains "$BASE/logs/tracked-install.out" 'Refusing to install the verifier'
! grep -qF '# >>> local gate >>>' .githooks/pre-push || { echo '[install-gate-test] tracked hook was modified'; exit 15; }
[ -z "$(git status --short)" ] || { echo '[install-gate-test] tracked hooksPath dirtied worktree'; git status --short; exit 16; }
printf '[install-gate-test] tracked hooksPath refused\n'

# A case-folded spelling of a tracked hook path is still tracked on case-insensitive filesystems.
make_repo "$BASE/tracked-hooks-case" yes
cd "$BASE/tracked-hooks-case"
git config core.hooksPath .GITHOOKS
if [ -d .GITHOOKS ]; then
  if bash tools/install-gate.sh > "$BASE/logs/tracked-case-install.out" 2>&1; then
    echo '[install-gate-test] case-folded tracked hooksPath install unexpectedly passed'
    exit 26
  fi
  assert_contains "$BASE/logs/tracked-case-install.out" 'Refusing to install the verifier'
  ! grep -qF '# >>> local gate >>>' .githooks/pre-push || { echo '[install-gate-test] case-folded tracked hook was modified'; exit 27; }
  [ -z "$(git status --short)" ] || { echo '[install-gate-test] case-folded tracked hooksPath dirtied worktree'; git status --short; exit 28; }
  printf '[install-gate-test] case-folded tracked hooksPath refused\n'
else
  printf '[install-gate-test] case-folded tracked hooksPath skipped on case-sensitive filesystem\n'
fi

# Windows-native absolute core.hooksPath should resolve to the real shell path when the platform exposes one.
make_repo "$BASE/winpath" yes
cd "$BASE/winpath"
win_hooks=""
if command -v cygpath >/dev/null 2>&1; then
  win_hooks="$(cygpath -w "$PWD/.git/hooks")"
else
  case "$PWD" in
    /mnt/[A-Za-z]/*)
      drive="${PWD#/mnt/}"
      drive="${drive%%/*}"
      rest="${PWD#/mnt/$drive/}"
      upper_drive="$(printf '%s' "$drive" | tr 'abcdefghijklmnopqrstuvwxyz' 'ABCDEFGHIJKLMNOPQRSTUVWXYZ')"
      win_hooks="$upper_drive:\\$(printf '%s' "$rest/.git/hooks" | tr '/' '\\')"
      ;;
  esac
fi
if [ -n "$win_hooks" ]; then
  git config core.hooksPath "$win_hooks"
  bash tools/install-gate.sh > "$BASE/logs/winpath-install.out"
  [ -f .git/hooks/pre-push ] || { echo '[install-gate-test] Windows hooksPath missed real .git/hooks/pre-push'; cat "$BASE/logs/winpath-install.out"; exit 17; }
  assert_contains .git/hooks/pre-push '# >>> local gate >>>'
  [ ! -d "$PWD/$win_hooks" ] || { echo '[install-gate-test] created repo-prefixed Windows path directory'; exit 18; }
  printf '[install-gate-test] Windows-native hooksPath normalized\n'
else
  printf '[install-gate-test] Windows-native hooksPath skipped on this platform\n'
fi

# Relative custom untracked hook paths remain supported.
make_repo "$BASE/custom" yes
cd "$BASE/custom"
git config core.hooksPath custom-hooks
bash tools/install-gate.sh > "$BASE/logs/custom-install.out"
[ -f custom-hooks/pre-push ] || { echo '[install-gate-test] relative custom hook not installed'; exit 19; }
assert_contains custom-hooks/pre-push '# >>> local gate >>>'
printf '[install-gate-test] relative custom hooksPath installed\n'

# Husky-v9 style .husky/_ hook path remains supported because the active hook is untracked.
make_repo "$BASE/husky" yes
cd "$BASE/husky"
git config core.hooksPath .husky/_
bash tools/install-gate.sh > "$BASE/logs/husky-install.out"
[ -f .husky/_/pre-push ] || { echo '[install-gate-test] Husky hook not installed'; exit 20; }
assert_contains .husky/_/pre-push '# >>> local gate >>>'
printf '[install-gate-test] Husky-v9 hooksPath installed\n'

# R4-01: in a LINKED worktree the git common dir can live on a DIFFERENT filesystem than the worktree.
# Case-sensitivity must be decided on the WORKTREE's FS (which hosts the tracked .githooks), not the common
# dir — otherwise a case-insensitive worktree whose common dir is case-sensitive mis-detects "sensitive",
# drops the icase check, and `.GITHOOKS` aliases the tracked hook. We can't portably force the cross-FS split
# here, but the fix tests the tracked hook directly (never the common dir), so the linked-worktree STRUCTURE
# is the regression: a case-folded tracked hooksPath inside a linked worktree must still refuse.
make_repo "$BASE/wt-base" yes
cd "$BASE/wt-base"
git branch -q wt-branch
wt_dir="$BASE/wt-linked"
if git worktree add -q "$wt_dir" wt-branch 2>/dev/null; then
  cd "$wt_dir"
  if [ -d .GITHOOKS ]; then
    git config core.hooksPath .GITHOOKS
    if bash tools/install-gate.sh > "$BASE/logs/wt-install.out" 2>&1; then
      echo '[install-gate-test] linked-worktree case-fold install unexpectedly passed'; exit 30
    fi
    assert_contains "$BASE/logs/wt-install.out" 'Refusing to install the verifier'
    printf '[install-gate-test] linked-worktree case-fold tracked hooksPath refused\n'
  else
    printf '[install-gate-test] linked-worktree case-fold skipped on case-sensitive filesystem\n'
  fi
else
  printf '[install-gate-test] linked-worktree add unsupported — skipped\n'
fi

# R5-01: a repo-root path segment differing only by NON-ASCII case (Ü vs ü) defeated the ASCII-only
# string matching in _vg_rel_under_root — an absolute core.hooksPath spelled with the case-variant aliased
# the tracked .githooks/pre-push while the relative-path derivation returned empty, so the refusal was
# skipped and install dirtied the tracked hook. The filesystem-identity check (git resolves the real dir)
# is immune. Only meaningful on a filesystem that folds non-ASCII case (macOS/APFS, Windows/NTFS).
uc_base="$BASE/uc"
mkdir -p "$uc_base"
if ( cd "$uc_base" && mkdir -p "ÜRepo" 2>/dev/null && [ -e "üRepo" ] ) 2>/dev/null; then
  uc_repo="$uc_base/ÜRepo"; uc_alt="$uc_base/üRepo"
  (
    cd "$uc_repo" || exit 1
    git init -q; git config user.email audit@example.test; git config user.name audit
    mkdir -p tools .githooks .gate
    cp "$ROOT/tools/install-gate.sh" tools/install-gate.sh
    cp "$ROOT/tools/test-install-gate.sh" tools/test-install-gate.sh
    cp "$ROOT/.githooks/pre-push" .githooks/pre-push
    cp "$ROOT/.gate/gate.json" .gate/gate.json
    chmod +x tools/install-gate.sh tools/test-install-gate.sh .githooks/pre-push
    git add -A; git commit -qm init
  ) >/dev/null 2>&1
  cd "$uc_repo"
  git config core.hooksPath "$uc_alt/.GITHOOKS"
  if bash tools/install-gate.sh > "$BASE/logs/uc-install.out" 2>&1; then
    echo '[install-gate-test] non-ASCII-case root install unexpectedly passed'; exit 32
  fi
  assert_contains "$BASE/logs/uc-install.out" 'Refusing to install the verifier'
  printf '[install-gate-test] non-ASCII-case root (R5-01) refused\n'
else
  printf '[install-gate-test] non-ASCII-case root (R5-01) skipped (FS does not fold non-ASCII case)\n'
fi

# R6-01: `.githooks` as a tracked SUBMODULE. The submodule's own git toplevel is NOT $ROOT, so the plain
# dir-identity check misses it, but its content is branch-controlled via the gitlink. Must refuse.
sub_src="$BASE/subhooks-src"
mkdir -p "$sub_src"
(
  cd "$sub_src" || exit 1
  git init -q; git config user.email audit@example.test; git config user.name audit
  printf '#!/bin/bash\necho PWNED\nexit 0\n' > pre-push; chmod +x pre-push
  git add -A; git commit -qm subinit
) >/dev/null 2>&1
make_repo "$BASE/submod" yes
cd "$BASE/submod"
if git -c protocol.file.allow=always submodule add -q "$sub_src" .subhooks >/dev/null 2>&1; then
  git -c protocol.file.allow=always commit -qm "add submodule hooks" >/dev/null 2>&1
  git config core.hooksPath .subhooks
  if bash tools/install-gate.sh > "$BASE/logs/submod-install.out" 2>&1; then
    echo '[install-gate-test] submodule hook dir install unexpectedly passed'; exit 34
  fi
  assert_contains "$BASE/logs/submod-install.out" 'Refusing to install the verifier'
  printf '[install-gate-test] submodule hook dir (R6-01) refused\n'
else
  printf '[install-gate-test] submodule hook dir (R6-01) skipped (submodule add unsupported)\n'
fi

# R7: nested submodule hook dirs must walk the full superproject chain. Git reports the immediate parent
# submodule first, not the root repo, but the root gitlink still controls the reachable hook content.
outer_src="$BASE/nested-outer-src"
mkdir -p "$outer_src"
(
  cd "$outer_src" || exit 1
  git init -q; git config user.email audit@example.test; git config user.name audit
  git -c protocol.file.allow=always submodule add -q "$sub_src" nested
  git commit -qm outerinit
) >/dev/null 2>&1
make_repo "$BASE/nested-submod" yes
cd "$BASE/nested-submod"
if git -c protocol.file.allow=always submodule add -q "$outer_src" outer >/dev/null 2>&1 \
   && git -c protocol.file.allow=always commit -qm "add outer submodule" >/dev/null 2>&1 \
   && git -c protocol.file.allow=always submodule update --init --recursive >/dev/null 2>&1; then
  git config core.hooksPath outer/nested
  if bash tools/install-gate.sh > "$BASE/logs/nested-submod-install.out" 2>&1; then
    echo '[install-gate-test] nested submodule hook dir install unexpectedly passed'; exit 36
  fi
  assert_contains "$BASE/logs/nested-submod-install.out" 'Refusing to install the verifier'
  ! grep -qF '# >>> local gate >>>' outer/nested/pre-push || { echo '[install-gate-test] nested submodule hook was modified'; exit 37; }
  printf '[install-gate-test] nested submodule hook dir (R7) refused\n'
else
  printf '[install-gate-test] nested submodule hook dir (R7) skipped (recursive submodule unsupported)\n'
fi

# R7: the active hook is always named pre-push, but it may be a symlink to a tracked file with another
# basename. The identity check must scan all tracked files, not only tracked paths matching *pre-push.
make_repo "$BASE/symlink-target" yes
cd "$BASE/symlink-target"
printf '#!/bin/bash\necho TRACKED_PAYLOAD\n' > .githooks/payload
chmod +x .githooks/payload
git add .githooks/payload
git commit -qm "add tracked hook payload"
git config core.hooksPath custom-hooks
mkdir -p custom-hooks
if ln -s ../.githooks/payload custom-hooks/pre-push 2>/dev/null && [ -L custom-hooks/pre-push ]; then
  if bash tools/install-gate.sh > "$BASE/logs/symlink-target-install.out" 2>&1; then
    echo '[install-gate-test] symlinked tracked hook target install unexpectedly passed'; exit 38
  fi
  assert_contains "$BASE/logs/symlink-target-install.out" 'Refusing to install the verifier'
  ! grep -qF '# >>> local gate >>>' .githooks/payload || { echo '[install-gate-test] symlink target was modified'; exit 39; }
  git diff --quiet -- .githooks/payload || { echo '[install-gate-test] symlink target dirtied worktree'; exit 40; }
  printf '[install-gate-test] symlinked tracked hook target (R7) refused\n'
else
  printf '[install-gate-test] symlinked tracked hook target (R7) skipped (symlink unsupported)\n'
fi
