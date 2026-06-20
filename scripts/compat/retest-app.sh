#!/usr/bin/env bash
# Re-verify Victauri compatibility against ONE real-world Tauri app.
#
# Clones the app at its pinned ref, injects the *current* victauri-plugin (as a
# path dependency on this repo), builds the frontend + a debug Tauri binary,
# launches it headless, and runs the app-agnostic smoke battery (smoke.sh).
#
# Usage:
#   scripts/compat/retest-app.sh <app-key> [--keep]
#     <app-key>   one of the keys in scripts/compat/apps.json
#     --keep      don't delete the clone/work dir afterwards (for debugging)
#
# Output: human-readable log, plus a final JSON line:
#   {"app":"...","ref":"...","victauri":"0.7.5","stage":"smoke","checks":N,"passed":P,"failed":F}
# where "stage" is the furthest stage reached (clone|frontend|build|launch|smoke).
#
# Exit code: 0 only if the app built, launched, and every smoke check passed.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
plugin_path="$repo_root/crates/victauri-plugin"
victauri_version="$(grep -m1 '^version' "$repo_root/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"

key="${1:-}"
keep="${2:-}"
[ -n "$key" ] || { echo "usage: retest-app.sh <app-key> [--keep]"; exit 2; }

cfg="$here/apps.json"
app=$(jq -r --arg k "$key" '.apps[] | select(.key==$k)' "$cfg")
[ -n "$app" ] || { echo "::error::unknown app '$key' (see $cfg)"; exit 2; }

repo=$(jq -r '.repo'           <<<"$app")
ref=$(jq -r '.ref'             <<<"$app")
fe_build=$(jq -r '.frontend_build' <<<"$app")
tauri_dir=$(jq -r '.tauri_dir' <<<"$app")
name=$(jq -r '.name'           <<<"$app")

stage="clone"
emit() { # emit <checks> <passed> <failed>
  echo "{\"app\":\"$key\",\"name\":\"$name\",\"ref\":\"${ref:0:12}\",\"victauri\":\"$victauri_version\",\"stage\":\"$stage\",\"checks\":${1:-0},\"passed\":${2:-0},\"failed\":${3:-0}}"
}
die() { echo "::error::[$key] $1"; emit 0 0 0; exit 1; }

# Create the work dir (which holds app.log) under $RUNNER_TEMP in CI so the
# `upload-artifact` glob never has to scan all of /tmp — globbing `/tmp/**` trips over
# the root-owned `/tmp/snap-private-tmp` (EACCES) and fails the whole job even when the
# smoke battery passed. $RUNNER_TEMP is runner-owned and snap-free; falls back to /tmp
# for local runs (which have no upload step).
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/victauri-compat.XXXXXX")"
cleanup() {
  [ -n "${APP_PID:-}" ] && kill "$APP_PID" 2>/dev/null || true
  [ "$keep" = "--keep" ] || rm -rf "$work"
}
trap cleanup EXIT

echo "================ Victauri compat retest: $name ================"
echo "repo=$repo ref=$ref  victauri=$victauri_version"

# ── 1. Clone at the pinned ref ───────────────────────────────────────────────
echo "--- clone ---"
git clone --quiet --filter=blob:none "$repo" "$work/app" || die "git clone failed"
git -C "$work/app" checkout --quiet "$ref" || die "checkout $ref failed"
app_dir="$work/app"
td="$app_dir/$tauri_dir"
[ -f "$td/Cargo.toml" ] || die "no Cargo.toml at $tauri_dir"

# ── 2. Inject the current victauri-plugin ────────────────────────────────────
echo "--- inject victauri-plugin ---"
# 2a. Cargo dependency (path dep on THIS repo's plugin).
if ! grep -q 'victauri-plugin' "$td/Cargo.toml"; then
  awk -v p="$plugin_path" '
    /^\[dependencies\]/ && !done {
      print; print "victauri-plugin = { path = \"" p "\", default-features = false }"; done=1; next
    } { print }
  ' "$td/Cargo.toml" > "$td/Cargo.toml.new" && mv "$td/Cargo.toml.new" "$td/Cargo.toml"
fi

# 2b. .plugin(victauri_plugin::init()) right after the Tauri builder is created.
builder_file=$(grep -rlE 'tauri::Builder::(default|new)\(\)' "$td/src" | head -1)
[ -n "$builder_file" ] || die "could not find 'tauri::Builder::default()' under $tauri_dir/src to inject the plugin"
if ! grep -q 'victauri_plugin::init' "$builder_file"; then
  perl -0pi -e 's/(tauri::Builder::(?:default|new)\(\))/$1\n        .plugin(victauri_plugin::init())/' "$builder_file"
fi
echo "injected into: ${builder_file#"$app_dir/"}"

# 2c. Capability granting victauri:default to all windows.
mkdir -p "$td/capabilities"
cat > "$td/capabilities/victauri.json" <<'JSON'
{
  "identifier": "victauri",
  "description": "Victauri compat retest -- debug only",
  "context": "local",
  "windows": ["*"],
  "permissions": ["victauri:default"]
}
JSON

# ── 3. Build the frontend (must precede cargo build; debug embeds frontendDist)
echo "--- frontend build ---"
stage="frontend"
( cd "$app_dir" && eval "$fe_build" ) || die "frontend build failed: $fe_build"

# ── 4. Build the debug Tauri binary; capture the produced executable path ─────
echo "--- cargo build (debug) ---"
stage="build"
build_log="$work/build.json"
( cd "$td" && cargo build --message-format=json ) > "$build_log" 2>"$work/build.err" || {
  tail -40 "$work/build.err"; die "cargo build failed"
}
bin=$(jq -rs '[.[] | select(.reason=="compiler-artifact" and .executable!=null) | .executable] | last' "$build_log")
[ -n "$bin" ] && [ -x "$bin" ] || die "no executable produced by cargo build"
echo "built: $bin"

# ── 5. Launch headless and wait for the embedded server + an eval-able webview
echo "--- launch ---"
stage="launch"
export WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
xvfb-run -a --server-args="-screen 0 1280x800x24" "$bin" > "$work/app.log" 2>&1 &
APP_PID=$!

# Victauri binds 7373 by default but falls back through 7374-7383 if the port is
# taken (and honours VICTAURI_PORT). Discover the actual port instead of assuming
# 7373, so an app that already uses 7373 isn't a false compatibility failure.
BASE=""
ports="${VICTAURI_PORT:-$(seq 7373 7383)}"
for _ in $(seq 1 90); do
  for p in $ports; do
    if curl -sf "http://127.0.0.1:$p/health" >/dev/null 2>&1; then
      BASE="http://127.0.0.1:$p"
      break
    fi
  done
  [ -n "$BASE" ] && break
  kill -0 "$APP_PID" 2>/dev/null || { echo "--- app.log ---"; tail -40 "$work/app.log"; die "app exited before the server came up"; }
  sleep 1
done
[ -n "$BASE" ] || { tail -40 "$work/app.log"; die "MCP server never became reachable on 127.0.0.1:7373-7383"; }
echo "server reachable at $BASE"

# Auth is ON by default — read the auto-generated Bearer token the plugin wrote to
# its discovery dir (`<temp>/victauri/<pid>/token`), exactly like a real client, so
# the smoke battery can authenticate. Only one app runs at a time, so the newest
# token file is ours. Without this every /api/tools call 401s and nothing passes.
VICTAURI_TOKEN="$(cat "$(ls -t "${TMPDIR:-/tmp}"/victauri/*/token /tmp/victauri/*/token 2>/dev/null | head -1)" 2>/dev/null || true)"
export VICTAURI_TOKEN
if [ -n "$VICTAURI_TOKEN" ]; then echo "discovered auth token (${#VICTAURI_TOKEN} chars)"; else echo "WARNING: no auth token found — tool calls may 401"; fi

# Wait for the webview to be eval-able (cold WebView/WebKit init can lag the server).
webview_ready=0
for _ in $(seq 1 45); do
  if curl -sf -X POST "$BASE/api/tools/eval_js" \
    -H 'content-type: application/json' -H "Authorization: Bearer $VICTAURI_TOKEN" \
    --data-binary '{"code":"return 1"}' 2>/dev/null | grep -q '"result"'; then
    webview_ready=1
    break
  fi
  sleep 2
done
if [ "$webview_ready" = 1 ]; then
  echo "webview eval-able"
else
  echo "WARNING: webview never became eval-able in ~90s — the page likely failed to load"
fi

# ── 6. Run the app-agnostic smoke battery ────────────────────────────────────
echo "--- smoke battery ---"
stage="smoke"
smoke_out="$work/smoke.out"
bash "$here/smoke.sh" "$BASE" | tee "$smoke_out"
summary=$(tail -1 "$smoke_out")
checks=$(jq -r '.checks' <<<"$summary" 2>/dev/null || echo 0)
passed=$(jq -r '.passed' <<<"$summary" 2>/dev/null || echo 0)
failed=$(jq -r '.failed' <<<"$summary" 2>/dev/null || echo 0)

emit "$checks" "$passed" "$failed"
# On any smoke failure dump the app's own stdout/stderr (webview console, load errors,
# WebKit warnings) so a "bridge not responding" failure is diagnosable from the job log
# alone — the app.log artifact is only uploaded on a clean exit.
if [ "${failed:-1}" -ne 0 ] || [ "${checks:-0}" -eq 0 ]; then
  echo "--- app.log (last 80 lines — why the webview/page misbehaved) ---"
  tail -80 "$work/app.log" 2>/dev/null || echo "(no app.log)"
  exit 1
fi
