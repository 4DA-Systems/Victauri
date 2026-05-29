#!/bin/bash
# Adversarial / limits / weakness probe against running 4DA (Victauri HEAD).
# Goal: find where Victauri breaks, lies, or falls short of CDP/Playwright.
# Unlike the happy-path harness, this RECORDS raw responses for manual scrutiny.
BASE="${VICTAURI_BASE:-http://127.0.0.1:7373}"
OUT="$(cd "$(dirname "$0")" && pwd)/adversarial-results.txt"
: > "$OUT"

tool() {
  local name="$1"; local body="${2:-"{}"}"
  curl -s -m 40 -X POST "$BASE/api/tools/$name" -H "Content-Type: application/json" -d "$body" 2>/dev/null
}
log() { echo -e "$1" | tee -a "$OUT"; }
probe() { # desc, tool, body  -> records raw response
  local desc="$1"; local name="$2"; local body="$3"
  local r; r=$(tool "$name" "$body")
  log "── $desc"
  log "   req: $name $body"
  log "   res: $(echo "$r" | jq -c '.' 2>/dev/null | head -c 400 || echo "$r" | head -c 400)"
}

log "###################### ADVERSARIAL PROBE ######################"
log "started: $(date)"

# Discover a real ref + db + command set up front
SNAP=$(tool dom_snapshot '{"format":"json"}')
REF=$(echo "$SNAP" | jq -r '.result | (.. | .ref? // empty)' 2>/dev/null | head -1)
log "discovered ref: $REF"

log "\n========== A. EVAL_JS TRUE LIMITS =========="
probe "exception thrown" eval_js '{"code":"throw new Error(\"boom\")"}'
probe "syntax error" eval_js '{"code":"return ;;; @@@ not js"}'
probe "undefined return" eval_js '{"code":"undefined"}'
probe "null return" eval_js '{"code":"null"}'
probe "NaN/Infinity" eval_js '{"code":"return [NaN, Infinity, -Infinity]"}'
probe "circular ref (non-serializable)" eval_js '{"code":"const o={}; o.self=o; return o"}'
probe "function return (non-serializable)" eval_js '{"code":"return function(){}"}'
probe "DOM node return" eval_js '{"code":"return document.body"}'
probe "BigInt return" eval_js '{"code":"return 123n"}'
probe "Symbol return" eval_js '{"code":"return Symbol(\"x\")"}'
probe "unicode/emoji" eval_js '{"code":"return \"日本語 🎉 \\u0000 \\ud83d\\ude00\""}'
probe "5MB+ output (size limit?)" eval_js '{"code":"return \"x\".repeat(6*1024*1024)"}'
probe "deeply nested object" eval_js '{"code":"let o={}; let c=o; for(let i=0;i<5000;i++){c.n={};c=c.n} return o"}'
probe "promise rejection" eval_js '{"code":"return Promise.reject(new Error(\"async boom\"))"}'
probe "async never resolves (30s timeout)" eval_js '{"code":"return new Promise(()=>{})"}'

log "\n========== B. QUERY_DB SECURITY (read-only enforcement) =========="
DBPATH=$(tool list_app_dir '{"pattern":"*.db","max_depth":3}' | jq -r '.result.entries[]?.path // empty' 2>/dev/null | head -1)
DBSQ=$(tool list_app_dir '{"pattern":"*.sqlite","max_depth":3}' | jq -r '.result.entries[]?.path // empty' 2>/dev/null | head -1)
log "discovered db: '$DBPATH' / '$DBSQ'"
probe "list tables (sqlite_master)" query_db '{"query":"SELECT name FROM sqlite_master WHERE type=\"table\" LIMIT 5"}'
probe "INSERT (should be blocked)" query_db '{"query":"INSERT INTO x VALUES (1)"}'
probe "UPDATE (should be blocked)" query_db '{"query":"UPDATE x SET a=1"}'
probe "DELETE (should be blocked)" query_db '{"query":"DELETE FROM x"}'
probe "DROP (should be blocked)" query_db '{"query":"DROP TABLE x"}'
probe "stacked query (SELECT; DROP)" query_db '{"query":"SELECT 1; DROP TABLE x"}'
probe "comment-hidden write" query_db '{"query":"SELECT 1 -- ; \nUPDATE x SET a=1"}'
probe "PRAGMA write (journal_mode)" query_db '{"query":"PRAGMA journal_mode=DELETE"}'
probe "ATTACH DATABASE (exfil vector)" query_db '{"query":"ATTACH DATABASE \"/tmp/evil.db\" AS evil"}'
probe "PRAGMA read (table_info)" query_db '{"query":"PRAGMA table_info(sqlite_master)"}'
probe "EXPLAIN" query_db '{"query":"EXPLAIN SELECT 1"}'
probe "bind params" query_db '{"query":"SELECT ?1 AS a, ?2 AS b","params":["hi",42]}'
probe "nonexistent db path" query_db '{"path":"does/not/exist.db","query":"SELECT 1"}'

log "\n========== C. READ_APP_FILE / LIST_APP_DIR TRAVERSAL =========="
probe "traversal ../../" read_app_file '{"path":"../../../../../../Windows/System32/drivers/etc/hosts"}'
probe "traversal config dir" read_app_file '{"directory":"config","path":"../../../../etc/passwd"}'
probe "absolute path" read_app_file '{"path":"C:/Windows/System32/drivers/etc/hosts"}'
probe "nonexistent file" read_app_file '{"path":"nope-does-not-exist.xyz"}'
probe "list traversal" list_app_dir '{"path":"../../../../","max_depth":1}'
probe "glob *partial* (primitive matcher?)" list_app_dir '{"pattern":"*set*","max_depth":2}'
probe "glob question mark" list_app_dir '{"pattern":"set?ings.json"}'

log "\n========== D. INVOKE_COMMAND ERROR SURFACING =========="
probe "nonexistent command" invoke_command '{"command":"this_does_not_exist_xyz"}'
probe "real command no args" invoke_command '{"command":"get_settings"}'
probe "command missing required args" invoke_command '{"command":"set_llm_provider"}'
probe "command bad arg types" invoke_command '{"command":"set_llm_provider","args":{"provider":12345}}'
probe "injection in command name" invoke_command '{"command":"get_settings\");evil(\""}'

log "\n========== E. NAVIGATE DANGEROUS URLS =========="
probe "javascript: URL" navigate '{"action":"go_to","url":"javascript:alert(1)"}'
probe "file:// URL" navigate '{"action":"go_to","url":"file:///C:/Windows/System32/drivers/etc/hosts"}'
probe "data: URL" navigate '{"action":"go_to","url":"data:text/html,<h1>x</h1>"}'

log "\n========== F. INTERACT/INPUT EDGE =========="
probe "stale/fake ref click" interact '{"action":"click","ref_id":"e99999"}'
probe "select_option on non-select" interact "{\"action\":\"select_option\",\"ref_id\":\"$REF\",\"value\":\"x\"}"
probe "fill non-input" input "{\"action\":\"fill\",\"ref_id\":\"$REF\",\"value\":\"x\"}"
probe "press invalid key" input '{"action":"press_key","key":"NotARealKey123"}'
probe "click missing ref_id" interact '{"action":"click"}'

log "\n========== G. WINDOW EDGE (non-destructive) =========="
probe "get_state nonexistent window" window '{"action":"get_state","label":"ghost_window"}'
probe "resize 0x0" window '{"action":"resize","label":"main","width":0,"height":0}'
probe "set_title unicode" window '{"action":"set_title","label":"main","title":"🎉 タイトル"}'
probe "manage bad action" window '{"action":"manage","label":"main","manage_action":"explode"}'

log "\n========== H. FAULT INJECTION EFFICACY =========="
tool fault '{"action":"clear_all"}' >/dev/null
probe "inject error fault on get_settings" fault '{"action":"inject","command":"get_settings","fault_type":"error","error_message":"INJECTED_FAULT"}'
probe "  -> invoke get_settings (should error)" invoke_command '{"command":"get_settings"}'
probe "list faults" fault '{"action":"list"}'
tool fault '{"action":"clear_all"}' >/dev/null
probe "inject delay 2000ms" fault '{"action":"inject","command":"get_settings","fault_type":"delay","delay_ms":2000}'
log "   timing delayed invoke:"
T0=$(date +%s%N); tool invoke_command '{"command":"get_settings"}' >/dev/null; T1=$(date +%s%N)
log "   elapsed: $(( (T1-T0)/1000000 ))ms (expect >=2000 if fault fired)"
tool fault '{"action":"clear_all"}' >/dev/null
probe "inject corrupt" fault '{"action":"inject","command":"get_settings","fault_type":"corrupt"}'
probe "  -> invoke (corrupted?)" invoke_command '{"command":"get_settings"}'
tool fault '{"action":"clear_all"}' >/dev/null

log "\n========== I. RECORDING ERROR STATES =========="
tool recording '{"action":"stop"}' >/dev/null 2>&1
probe "stop without start" recording '{"action":"stop"}'
probe "checkpoint without start" recording '{"action":"checkpoint","checkpoint_id":"c1"}'
probe "import malformed JSON" recording '{"action":"import","session_json":"{not valid"}'
probe "events_between nonexistent" recording '{"action":"events_between","from":"nope","to":"nope2"}'

log "\n========== J. SECURITY HEADERS / GUARDS (auth disabled in 4DA) =========="
log "── DNS rebinding (bad Host header)"
log "   $(curl -s -m 5 -o /dev/null -w 'status=%{http_code}' -H 'Host: evil.com' "$BASE/info")"
log "── Origin guard (cross-origin)"
log "   $(curl -s -m 5 -o /dev/null -w 'status=%{http_code}' -H 'Origin: http://evil.com' "$BASE/info")"
log "── security headers on /info"
curl -s -m 5 -D - -o /dev/null "$BASE/info" | grep -iE "x-content-type|x-frame|content-security|cache-control" | sed 's/^/   /' | tee -a "$OUT"

log "\n========== K. MALFORMED INPUT ROBUSTNESS =========="
probe "wrong type for action enum" interact '{"action":12345}'
probe "missing required field" eval_js '{}'
log "── totally invalid JSON body"
log "   $(curl -s -m 5 -X POST "$BASE/api/tools/eval_js" -H 'Content-Type: application/json' -d 'not json at all')"
probe "unknown tool" no_such_tool '{}'
log "── extra unknown fields (should ignore or reject?)"
probe "  extra field" eval_js '{"code":"return 1","bogus_field":true,"another":42}'

log "\n###################### DONE ######################"
log "finished: $(date)"
echo "Results written to $OUT"
