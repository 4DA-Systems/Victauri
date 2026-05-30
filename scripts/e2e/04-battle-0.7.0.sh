#!/bin/bash
# Battle-test of all 0.6.0 fixes + 0.7.0 parity features against a running app.
# Health + memory checked after each section to pinpoint any destabilizing op.
# Usage: bash 04-battle-0.7.0.sh   (app must be running on :7373)
B="${VICTAURI_BASE:-http://127.0.0.1:7373}"
PASS=0; FAIL=0; OBS=""
t(){ curl -s -m 30 -X POST "$B/api/tools/$1" -H 'Content-Type: application/json' -d "$2" 2>/dev/null; }
tb(){ curl -s -m 30 -X POST "$B/api/tools/$1" -H 'Content-Type: application/json' --data-binary @- 2>/dev/null; }
chk(){ # desc, jq-expr, json
  local d="$1" expr="$2" json="$3"
  if echo "$json" | jq -e "$expr" >/dev/null 2>&1; then PASS=$((PASS+1)); echo "  PASS $d";
  else FAIL=$((FAIL+1)); echo "  FAIL $d  <- $(echo "$json" | jq -c . 2>/dev/null | head -c 160)"; fi
}
health(){ # section label
  local h mem
  h=$(curl -s -m3 -o /dev/null -w '%{http_code}' "$B/health" 2>/dev/null)
  mem=$(powershell -Command "[math]::Round((Get-Process fourda -ErrorAction SilentlyContinue).WS/1MB)" 2>/dev/null | tr -d '\r')
  echo ">>> after [$1]: health=$h mem=${mem}MB"
  [ "$h" != "200" ] && { echo "!!! APP DOWN after [$1] — STOPPING"; exit 9; }
}

echo "############ BATTLE TEST 0.7.0 ($(date)) ############"
curl -s -m3 "$B/info" | jq -c '{v:.version,cmds:.commands_registered}'

echo "===== A. 0.6.0 FIX REGRESSIONS ====="
chk "eval multi-stmt localStorage" '.result=="v1"' "$(t eval_js '{"code":"localStorage.setItem(\"bk\",\"v1\"); return localStorage.getItem(\"bk\")"}')"
chk "eval multi-stmt member-assign" '.result==10' "$(t eval_js '{"code":"window.__b={a:5}; window.__b.b=window.__b.a*2; return window.__b.b"}')"
chk "eval bare expr still works" '.result=="4DA"' "$(t eval_js '{"code":"document.title"}')"
chk "eval deep-300 no envelope leak" '(.result|tostring|contains("__victauri_ok"))|not' "$(t eval_js '{"code":"let o={};let c=o;for(let i=0;i<300;i++){c.n={};c=c.n}return o"}')"
chk "logs ipc (no 5MB fail)" '.error==null and (.result|type)=="array"' "$(t logs '{"action":"ipc"}')"
chk "logs network (no 5MB fail)" '.error==null and (.result|type)=="array"' "$(t logs '{"action":"network"}')"
chk "logs slow_ipc (no 5MB fail)" '.error==null' "$(t logs '{"action":"slow_ipc","threshold_ms":1}')"
chk "detect_ghost (no 5MB fail)" '.error==null' "$(t detect_ghost_commands '{}')"
chk "window get_state ghost -> error" '.error!=null' "$(t window '{"action":"get_state","label":"ghostzzz"}')"
chk "window resize 0x0 -> error" '.error!=null' "$(t window '{"action":"resize","label":"main","width":0,"height":0}')"
DBT=$(t query_db '{"path":"4da.db","query":"SELECT name FROM sqlite_master WHERE type=\"table\" LIMIT 3"}')
chk "query_db reaches real 4da.db (db_search_paths)" '.result.rows|length>0' "$DBT"
chk "query_db PRAGMA write blocked" '.error|test("PRAGMA writes")' "$(t query_db '{"path":"4da.db","query":"PRAGMA user_version=7"}')"
health "A fixes"

echo "===== B. ROUTE (network interception) ====="
t route '{"action":"clear_all"}' >/dev/null
chk "route add fulfill" '.result.ok==true' "$(t route '{"action":"add","pattern":"bt.example","behavior":"fulfill","status":418,"body":{"ok":1}}')"
chk "fulfill returns mock" '.result.status==418 and .result.body.ok==1' "$(t eval_js '{"code":"return fetch(\"https://bt.example/x\").then(r=>r.json().then(j=>({status:r.status,body:j})))"}')"
t route '{"action":"add","pattern":"btblock.example","behavior":"block"}' >/dev/null
chk "block rejects" '.result|test("BLOCKED|blocked")' "$(t eval_js '{"code":"return fetch(\"https://btblock.example/x\").then(()=>\"no\").catch(e=>\"BLOCKED:\"+e.message)"}')"
t route '{"action":"add","pattern":"btglob","behavior":"fulfill","status":201,"match_type":"glob","body":"X"}' >/dev/null 2>&1
t route '{"action":"add","pattern":"btdelay.example","behavior":"delay","delay_ms":1200}' >/dev/null
chk "delay >=1200ms" '.result>=1200' "$(t eval_js '{"code":"const t=Date.now();return fetch(\"https://btdelay.example/x\").then(()=>Date.now()-t).catch(()=>Date.now()-t)"}')"
chk "route matches logged" '.result|length>=3' "$(t route '{"action":"matches"}')"
chk "route list" '.result|length>=3' "$(t route '{"action":"list"}')"
chk "route clear_all" '.result.removed>=3' "$(t route '{"action":"clear_all"}')"
health "B route"

echo "===== C. TRACE (screencast) ====="
t window '{"action":"manage","label":"main","manage_action":"unminimize"}' >/dev/null 2>&1
t window '{"action":"manage","label":"main","manage_action":"show"}' >/dev/null 2>&1
sleep 0.5
chk "trace start" '.result.started==true' "$(t trace '{"action":"start","interval_ms":300,"max_frames":15,"with_events":true}')"
sleep 1.4
chk "trace status active" '.result.active==true and .result.frame_count>=2' "$(t trace '{"action":"status"}')"
chk "trace stop summary" '.result.frame_count>=2 and .result.recorded_event_count>=0' "$(t trace '{"action":"stop"}')"
chk "trace frames are PNGs" '(.result|type)=="array" and (.result[0].data[0:8]=="iVBORw0K")' "$(t trace '{"action":"frames","limit":2}')"
health "C trace"

echo "===== D. TRUSTED INPUT (Win32, needs foreground) ====="
t window '{"action":"manage","label":"main","manage_action":"focus"}' >/dev/null 2>&1
sleep 0.4
tb eval_js >/dev/null <<'JSON'
{"code":"(function(){['bti','btn'].forEach(id=>{var o=document.getElementById(id);if(o)o.remove()}); var i=document.createElement('input'); i.id='bti'; window.__bk=[]; i.addEventListener('keydown',e=>window.__bk.push(e.isTrusted)); var b=document.createElement('button'); b.id='btn'; b.textContent='BT'; b.style.cssText='position:fixed;top:150px;left:80px;width:120px;height:40px;z-index:99999'; window.__bc=null; b.addEventListener('click',e=>window.__bc=e.isTrusted); document.body.insertBefore(i,document.body.firstChild); document.body.appendChild(b); return 'ready';})()"}
JSON
IREF=$(t find_elements '{"css":"#bti"}' | jq -r '.result[0].ref_id')
t input "{\"action\":\"type_text\",\"ref_id\":\"$IREF\",\"text\":\"Hi\",\"trusted\":true}" >/dev/null
sleep 0.4
chk "trusted type: value set" '.result=="Hi"' "$(t eval_js '{"code":"return document.getElementById(\"bti\").value"}')"
chk "trusted type: isTrusted=true" '(.result|length)>0 and (.result|all(.==true))' "$(t eval_js '{"code":"return window.__bk"}')"
t window '{"action":"manage","label":"main","manage_action":"focus"}' >/dev/null 2>&1; sleep 0.3
BREF=$(t find_elements '{"css":"#btn"}' | jq -r '.result[0].ref_id')
t interact "{\"action\":\"click\",\"ref_id\":\"$BREF\",\"trusted\":true}" >/dev/null
sleep 0.4
chk "trusted click: isTrusted=true" '.result==true' "$(t eval_js '{"code":"return window.__bc"}')"
health "D trusted input"

echo "===== E. IFRAME (4DA has none — inject same-origin) ====="
tb eval_js >/dev/null <<'JSON'
{"code":"(function(){var o=document.getElementById('btf');if(o)o.remove();var f=document.createElement('iframe');f.id='btf';document.body.appendChild(f);var d=f.contentDocument;var b=d.createElement('button');b.id='fbtn';b.textContent='FrameBT';b.onclick=function(){b.textContent='FCLICK'};d.body.appendChild(b);return 'iframe ready';})()"}
JSON
chk "snapshot sees iframe content" '.result.tree|test("iframe content")' "$(t dom_snapshot '{"format":"compact"}')"
FREF=$(t find_elements '{"css":"#fbtn"}' | jq -r '.result[0].ref_id // "NONE"')
chk "find frame element" "\"$FREF\"!=\"NONE\"" "$(echo "{\"r\":\"$FREF\"}")"
t interact "{\"action\":\"click\",\"ref_id\":\"$FREF\"}" >/dev/null
chk "click reaches frame element" '.result=="FCLICK"' "$(t eval_js '{"code":"return document.getElementById(\"btf\").contentDocument.getElementById(\"fbtn\").textContent"}')"
health "E iframe"

echo "===== F. EDGE CASES & LIMITS ====="
chk "eval 5MB cap fires" '.error|test("too large")' "$(t eval_js '{"code":"return \"x\".repeat(6*1024*1024)"}')"
# Syntax errors surface only as the 30s timeout (documented limitation); allow 35s.
chk "eval syntax err -> timeout msg" '.error|test("timed out")' "$(curl -s -m 35 -X POST "$B/api/tools/eval_js" -d '{"code":"return 1 +"}' 2>/dev/null)"
chk "eval unicode roundtrip" '.result=="日本語"' "$(printf '{"code":"return %s"}' "'日本語'" | tb eval_js)"
chk "find invalid selector -> error" '.error!=null' "$(t find_elements '{"css":">>bad"}')"
chk "route regex match_type" '.result.ok==true' "$(t route '{"action":"add","pattern":"^https://re\\\\.test/.*","match_type":"regex","behavior":"block"}')"
t route '{"action":"clear_all"}' >/dev/null
chk "read_app_file traversal blocked" '.error!=null' "$(t read_app_file '{"path":"../../../../Windows/System32/drivers/etc/hosts"}')"
chk "query_db stacked blocked" '.error|test("stacked")' "$(t query_db '{"path":"4da.db","query":"SELECT 1; DROP TABLE x"}')"
chk "invoke nonexistent cmd surfaces err" '.error!=null' "$(t invoke_command '{"command":"zzz_nope"}')"
chk "trace start/stop idempotent" '.result.stopped==true' "$(t trace '{"action":"stop"}')"
health "F edge cases"

echo "===== G. STRESS (watch stability) ====="
echo "  50 rapid evals..."; OK=0; for i in $(seq 1 50); do r=$(t eval_js '{"code":"return Math.random()"}' | jq -r '.result // "x"'); [ "$r" != "x" ] && OK=$((OK+1)); done
TOTAL_C=$((PASS+FAIL+1)); if [ "$OK" -ge 48 ]; then PASS=$((PASS+1)); echo "  PASS 50 rapid evals ($OK/50)"; else FAIL=$((FAIL+1)); echo "  FAIL 50 rapid evals ($OK/50)"; fi
echo "  20 concurrent evals..."; for i in $(seq 1 20); do t eval_js '{"code":"return 1+1"}' >/dev/null & done; wait
chk "concurrent evals survived" '.result==2' "$(t eval_js '{"code":"return 1+1"}')"
echo "  10 concurrent mixed tools..."; (t dom_snapshot '{"format":"compact"}' >/dev/null &); (t logs '{"action":"console"}' >/dev/null &); (t get_memory_stats '{}' >/dev/null &); (t introspect '{"action":"plugin_state"}' >/dev/null &); wait
chk "mixed concurrent survived" '.result!=null' "$(t get_memory_stats '{}')"
health "G stress"

echo ""
echo "############ RESULT: $PASS passed, $FAIL failed ############"
echo "finished: $(date)"
