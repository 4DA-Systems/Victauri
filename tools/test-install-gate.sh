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
assert_contains "$BASE/logs/tracked-install.out" 'active pre-push hook is tracked by this repo: .githooks/pre-push'
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
  assert_contains "$BASE/logs/tracked-case-install.out" 'active pre-push hook is tracked by this repo:'
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
    assert_contains "$BASE/logs/wt-install.out" 'active pre-push hook is tracked by this repo:'
    printf '[install-gate-test] linked-worktree case-fold tracked hooksPath refused\n'
  else
    printf '[install-gate-test] linked-worktree case-fold skipped on case-sensitive filesystem\n'
  fi
else
  printf '[install-gate-test] linked-worktree add unsupported — skipped\n'
fi
