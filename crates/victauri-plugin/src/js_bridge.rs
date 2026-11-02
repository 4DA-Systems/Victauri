/// JS bridge log capacity configuration.
pub struct BridgeCapacities {
    /// Maximum console log entries to retain.
    pub console_logs: usize,
    /// Maximum DOM mutation batches to retain.
    pub mutation_log: usize,
    /// Maximum network request entries to retain.
    pub network_log: usize,
    /// Maximum navigation history entries to retain.
    pub navigation_log: usize,
    /// Maximum dialog event entries to retain.
    pub dialog_log: usize,
    /// Maximum long task entries to retain.
    pub long_tasks: usize,
}

impl Default for BridgeCapacities {
    fn default() -> Self {
        Self {
            console_logs: 1000,
            mutation_log: 500,
            network_log: 1000,
            navigation_log: 200,
            dialog_log: 100,
            long_tasks: 100,
        }
    }
}

/// Generate the JS init script with custom log capacities.
#[must_use]
pub fn init_script(caps: &BridgeCapacities) -> String {
    format!(
        "\n(function() {{\
        \n    if (window.__VICTAURI__) return;\
        \n\
        \n    var CAP_CONSOLE = {console_logs};\
        \n    var CAP_MUTATION = {mutation_log};\
        \n    var CAP_NETWORK = {network_log};\
        \n    var CAP_NAVIGATION = {navigation_log};\
        \n    var CAP_DIALOG = {dialog_log};\
        \n    var CAP_LONG_TASKS = {long_tasks};\
        \n",
        console_logs = caps.console_logs,
        mutation_log = caps.mutation_log,
        network_log = caps.network_log,
        navigation_log = caps.navigation_log,
        dialog_log = caps.dialog_log,
        long_tasks = caps.long_tasks,
    )
    // Inject the crate version into the JS bridge's self-reported version so it ALWAYS
    // equals `get_plugin_info`'s `BRIDGE_VERSION` (= CARGO_PKG_VERSION). Previously the
    // JS version was a hand-maintained literal that the bump script find-replaced each
    // release — it silently drifted (stuck at 0.7.8 through 0.7.10), so `get_diagnostics`
    // reported a stale `bridge_version` and the startup self-check logged a false
    // "Bridge version mismatch" on every launch. Deriving it here makes drift impossible.
        + &INIT_SCRIPT_BODY
            .replace("__VICTAURI_BRIDGE_VERSION__", env!("CARGO_PKG_VERSION"))
            .replace("__VICTAURI_AGENT_KEY__", agent_key())
}

/// Per-process secret that unlocks the bridge's agent-only operations (clearing logs and route
/// rules, dialog auto-responses, animation scrub / sweep recording). It is embedded in the init script's closure and in the
/// scripts Victauri itself injects, never in anything page script can read, so a page cannot
/// silently remove the agent's block/mock rules or erase captured evidence.
#[doc(hidden)]
#[must_use]
pub fn agent_key() -> &'static str {
    static KEY: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| uuid::Uuid::new_v4().simple().to_string());
    KEY.as_str()
}

/// JS expression evaluating to the bridge's agent-only operations object (or `undefined` when
/// the bridge is not loaded). Only valid inside scripts Victauri injects. `pub` only so the
/// jsdom suite can build the real agent snippets.
#[doc(hidden)]
#[must_use]
pub fn agent_ops_js() -> String {
    format!("window.__VICTAURI__?._agent(\"{}\")", agent_key())
}

/// Eval-wrapper code that calls one agent-only bridge operation, `call` (e.g.
/// `scrubSeek(0.5)`), awaiting a returned promise; an object with `error` when the bridge is
/// not loaded. Used by the `animation` tool. `pub` only so the jsdom suite can run the real
/// snippets.
#[doc(hidden)]
#[must_use]
pub fn agent_op_call_js(call: &str) -> String {
    format!(
        "return await (function () {{ var o = {}; \
         return o ? o.{call} : {{ error: 'the Victauri bridge is not loaded in this page' }}; }})()",
        agent_ops_js()
    )
}

// ── Eval scripts ─────────────────────────────────────────────────────────────
//
// The scripts an agent eval injects, in order: the liveness probe, then the wrapper around the
// user code, then the parse check. Internal plumbing (`pub` only so the jsdom suite can drive
// the real scripts); ids are Victauri-generated UUIDs and nonces come from the bridge.

fn js_literal(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "null".to_string())
}

/// The pre-eval liveness probe: answers `"probe_ok:<page nonce>"` (just `"probe_ok"` without a
/// bridge). The id follows `id:` with no space, unlike the wrapper's `id: `. The callback args
/// have no prototype, so a page's `Object.prototype.toJSON` cannot break their serialization
/// (R5-JS2).
#[doc(hidden)]
#[must_use]
pub fn eval_probe_script(id: &str) -> String {
    format!(
        "(async()=>{{var v=window.__VICTAURI__;\
         var n=(v&&typeof v._pageNonce==='string')?':'+v._pageNonce:'';\
         await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback',\
         Object.assign(Object.create(null),{{id:{},result:'\"probe_ok'+n+'\"'}}));}})();",
        js_literal(id)
    )
}

/// The page nonce reported by a liveness-probe answer (`None` for a page without a nonce).
#[doc(hidden)]
#[must_use]
pub fn probe_answer_nonce(raw: &str) -> Option<String> {
    let answer: String = serde_json::from_str(raw).ok()?;
    answer
        .strip_prefix("probe_ok:")
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

/// The wrapper that runs `code` (already `return`-prefixed as needed) and settles its outcome
/// exactly once through the bridge.
#[doc(hidden)]
#[must_use]
pub fn eval_wrapper_script(id: &str, code: &str) -> String {
    let id_js = js_literal(id);
    // Code carrying the agent key (only Victauri's own agent-op snippets — the key is a
    // per-process secret) runs in a STRICT wrapper. In a sloppy one a page hook on anything the
    // snippet touches (a `then` getter consulted when the result settles, a built-in an op calls)
    // reaches the wrapper through `hook.caller` or V8 stack-frame `getFunction()`, and its source
    // holds the key (R4-JS2). Strict functions are censored from both. User code keeps sloppy
    // semantics (implicit globals etc.), so strictness is applied only where the key is.
    let strict = if code.contains(agent_key()) {
        "'use strict';"
    } else {
        ""
    };
    // `{code}` is followed by a NEWLINE so a trailing `// comment` in the user code cannot
    // comment out the rest of the wrapper (it used to turn every such eval into a parse error).
    // `_evalBegin` runs synchronously when the script is evaluated, before the parse check.
    format!(
        r"
        (async () => {{ {strict}
            const __vic = {{ id: {id_js}, bridge: window.__VICTAURI__ }};
            const __settle = (p) => (__vic.bridge && __vic.bridge._evalSettle)
                ? __vic.bridge._evalSettle(__vic.id, p)
                : window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                    id: __vic.id, result: JSON.stringify(p)
                }});
            if (__vic.bridge && __vic.bridge._evalBegin && !__vic.bridge._evalBegin(__vic.id)) return;
            try {{
                const __result = await (async () => {{ {code}
 }})();
                const __type = __result === undefined ? 'undefined'
                    : __result === null ? 'null' : 'value';
                const __val = __type === 'value' ? __result : null;
                await __settle({{ __victauri_ok: __val, __victauri_type: __type }});
            }} catch (e) {{
                await __settle({{ __victauri_err: (function (x) {{
                    // A Tauri command's `Err(serde struct)` rejects with a plain object:
                    // ``String(obj)`` would collapse it to '[object Object]'.
                    if (x && typeof x.message === 'string') return x.message;
                    if (typeof x === 'string') return x;
                    try {{ var s = JSON.stringify(x); if (s !== undefined) return s; }} catch (_) {{}}
                    try {{ return String(x); }} catch (_) {{ return Object.prototype.toString.call(x); }}
                }})(e) }});
            }}
        }})();
        "
    )
}

/// The parse check, delivered right after the wrapper to the same window. It reports "did not
/// begin executing" only if the wrapper never started in the page the eval was armed in
/// (`nonce`, from the liveness probe; `None` disables the check).
#[doc(hidden)]
#[must_use]
pub fn eval_check_script(id: &str, nonce: Option<&str>) -> String {
    format!(
        "(function () {{ var v = window.__VICTAURI__; \
         if (v && v._evalCheck) v._evalCheck({}, {}); }})();",
        js_literal(id),
        nonce.map_or_else(|| "null".to_string(), js_literal)
    )
}

/// The script the in-app `victauri_eval_js` command (a page-originated eval) injects: runs
/// `code` and delivers its result to `victauri_eval_callback` under `id_json` (a JSON string
/// literal). `pub` only so the jsdom suite can drive the real script.
#[doc(hidden)]
#[must_use]
pub fn page_eval_script(id_json: &str, code: &str) -> String {
    // The result body is built by the bridge's `_pageEvalBody` (the `JSON.stringify` captured at
    // init, blind to a `toJSON` planted on the universal prototypes, and never throwing), and the
    // callback args have no prototype, so Tauri's own serialization of them cannot hit a planted
    // `toJSON` either (R5B-PRISTINE1, mirroring R5-JS2 for agent evals). Without the bridge it
    // falls back to the page's JSON, as before. Result format is unchanged: the value's JSON
    // (`null` for `undefined`), or `{"__error": message}`.
    format!(
        r"
        (async () => {{
            const __vb = window.__VICTAURI__;
            const __body = (__vb && __vb._pageEvalBody)
                ? __vb._pageEvalBody
                : (v, isErr) => isErr
                    ? JSON.stringify({{ __error: v && v.message }})
                    : (JSON.stringify(v) ?? 'null');
            const __send = (body) => {{
                const a = Object.create(null);
                a.id = {id_json};
                a.result = body;
                return window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', a);
            }};
            let __out;
            try {{
                const __result = await (async () => {{ {code}
 }})();
                __out = __body(__result, false);
            }} catch (e) {{
                __out = __body(e, true);
            }}
            await __send(__out);
        }})();
        "
    )
}

/// The body of the init script (after capacity variable declarations).
/// Uses CAP_* variables for all log limits.
const INIT_SCRIPT_BODY: &str = r#"
    var refMap = new Map();
    var refCounter = 0;
    var weakRefMap = new Map();
    // Ref ids carry a tag unique to this page load (and window), so a ref noted before a reload,
    // HMR or in another window can never resolve here: with bare counters the same `e1` meant a
    // different element after any reload, and clicking it reported ok on the wrong element.
    // Grammar: 4 chars [a-z][0-9a-z]{3}, then `e`, then the counter (`k3f9e12`).
    var PAGE_TAG = (function() {
        var alphabet = 'abcdefghijklmnopqrstuvwxyz0123456789';
        var b = null;
        try { b = new Uint32Array(4); window.crypto.getRandomValues(b); } catch (e) { b = null; }
        function pick(i, n) { return alphabet.charAt((b ? b[i] : Math.floor(Math.random() * 4294967296)) % n); }
        return pick(0, 26) + pick(1, 36) + pick(2, 36) + pick(3, 36);
    })();
    var REF_PREFIX = PAGE_TAG + 'e';
    // A bare `e12` is this page's counter 12. No plugin since the tag was added issues one, so it
    // can only be a test or script written against the older form, never a stale ref held across
    // a reload.
    function isBareCounter(refId) {
        if (typeof refId !== 'string' || refId.length < 2 || refId.charAt(0) !== 'e') return false;
        for (var i = 1; i < refId.length; i++) {
            var c = refId.charCodeAt(i);
            if (c < 48 || c > 57) return false;
        }
        return true;
    }
    function isForeignRef(refId) {
        return typeof refId !== 'string'
            || (REFLECT_APPLY(STR_INDEXOF, refId, [REF_PREFIX]) !== 0 && !isBareCounter(refId));
    }
    function foreignRefError(refId) {
        return isForeignRef(refId)
            ? 'ref not found: ' + refId + ' (this ref is from a previous page load or another window; this page\'s refs start with '
                + REF_PREFIX + ' — take a new dom_snapshot or find_elements)'
            : null;
    }
    function refNotFound(refId) {
        return foreignRefError(refId) || 'ref not found: ' + refId;
    }

    function resolveRef(refId) {
        if (isBareCounter(refId)) refId = REF_PREFIX + refId.slice(1);
        var direct = REFLECT_APPLY(MAP_GET, refMap, [refId]);
        if (direct) {
            if (direct.isConnected) return direct;
            REFLECT_APPLY(MAP_DELETE, refMap, [refId]);
            return null;
        }
        var weak = REFLECT_APPLY(MAP_GET, weakRefMap, [refId]);
        if (weak) {
            var el = REFLECT_APPLY(WEAKREF_DEREF, weak, []);
            if (el && el.isConnected) return el;
            REFLECT_APPLY(MAP_DELETE, weakRefMap, [refId]);
            return null;
        }
        return null;
    }

    var REF_MAP_LIMIT = 10000;
    var HAS_WEAKREF = typeof WeakRef !== 'undefined';
    // node -> ref id. Re-snapshotting the same DOM reuses ids instead of minting a
    // fresh id per node per snapshot (which grew weakRefMap by the whole DOM on
    // every snapshot). A WeakMap never keeps a detached node alive.
    var nodeIds = new WeakMap();
    // Ids registered by the snapshot currently being built (null outside one).
    var buildingRefs = null;

    function registerRef(node) {
        var ref_id = REFLECT_APPLY(WEAKMAP_GET, nodeIds, [node]);
        if (ref_id === undefined) {
            ref_id = REF_PREFIX + (refCounter++);
            REFLECT_APPLY(WEAKMAP_SET, nodeIds, [node, ref_id]);
        }
        REFLECT_APPLY(MAP_DELETE, refMap, [ref_id]); // re-insert as newest
        if (REFLECT_APPLY(MAP_SIZE_GET, refMap, []) >= REF_MAP_LIMIT) {
            // Drop the oldest STRONG reference only. With WeakRef support the ref
            // stays resolvable through weakRefMap while its node is alive, so a
            // snapshot larger than the limit never invalidates its own early refs.
            var oldest = REFLECT_APPLY(MAP_KEYS, refMap, []).next().value;
            REFLECT_APPLY(MAP_DELETE, refMap, [oldest]);
            if (!HAS_WEAKREF) REFLECT_APPLY(MAP_DELETE, weakRefMap, [oldest]);
        }
        REFLECT_APPLY(MAP_SET, refMap, [ref_id, node]);
        if (HAS_WEAKREF) {
            var existing = REFLECT_APPLY(MAP_GET, weakRefMap, [ref_id]);
            if (!existing || REFLECT_APPLY(WEAKREF_DEREF, existing, []) !== node) {
                REFLECT_APPLY(MAP_SET, weakRefMap, [ref_id, new WEAKREF_CTOR(node)]);
            }
        }
        if (buildingRefs) buildingRefs[ref_id] = true;
        return ref_id;
    }

    function getStaleRefs() {
        var stale = [];
        REFLECT_APPLY(MAP_FOREACH, weakRefMap, [function(weak, refId) {
            var el = REFLECT_APPLY(WEAKREF_DEREF, weak, []);
            if (!el || !el.isConnected) {
                stale[stale.length] = refId;
                REFLECT_APPLY(MAP_DELETE, weakRefMap, [refId]);
                REFLECT_APPLY(MAP_DELETE, refMap, [refId]);
            }
        }]);
        return stale;
    }
    var consoleLogs = [];
    var mutationLog = [];
    var networkLog = [];
    var networkCounter = 0;
    // Tauri's IPC transport URL is platform-dependent: WebView2 (Windows) uses
    // `http://ipc.localhost/<cmd>`, while WebKitGTK (Linux) and WKWebView (macOS)
    // use the custom `ipc://localhost/<cmd>` scheme. Match BOTH so the IPC-derived
    // tools (getIpcLog / ghost detection / integrity / event stream) work on every
    // platform — not just Windows. Returns the (still URL-encoded) command path if
    // the URL is a Tauri IPC URL, else null.
    // `https://ipc.localhost/<cmd>` too: an app that sets `useHttpsScheme` (Windows, Android)
    // gets Tauri's `protocolScheme` 'https' (tauri manager/webview.rs, core.js convertFileSrc).
    var IPC_PREFIXES = ['http://ipc.localhost/', 'https://ipc.localhost/', 'ipc://localhost/'];
    // A spelling fetch accepts but the raw prefix check misses (`HTTP://IPC.LOCALHOST/x`,
    // `http://ipc.localhost:80/x`, a leading space, backslashes) is tried again on the URL as
    // the URL parser normalizes it. Only when it can be one (it names `ipc`), and not for huge
    // strings (a data: URL never is).
    var IPC_NORMALIZE_MAX = 8192;
    function ipcPrefixPath(url) {
        for (var pi = 0; pi < IPC_PREFIXES.length; pi++) {
            if (REFLECT_APPLY(STR_INDEXOF, url, [IPC_PREFIXES[pi]]) === 0) {
                return REFLECT_APPLY(STR_SUBSTRING, url, [IPC_PREFIXES[pi].length]);
            }
        }
        return null;
    }
    function ipcCommandPath(url) {
        if (typeof url !== 'string') {
            try { url = String(url); } catch (e) { return null; }
        }
        var p = ipcPrefixPath(url);
        if (p !== null || url.length > IPC_NORMALIZE_MAX || !URL_CTOR || !URL_HREF_GET) return p;
        if (REFLECT_APPLY(STR_INDEXOF, REFLECT_APPLY(STR_TO_LOWER, url, []), ['ipc']) === -1) return null;
        try { return ipcPrefixPath(REFLECT_APPLY(URL_HREF_GET, new URL_CTOR(url), [])); } catch (e) { return null; }
    }
    function isIpcUrl(url) { return ipcCommandPath(url) !== null; }
    // The URL as stored in the network / route-match logs (R5B-DATAURL1). A log holds up to
    // CAP_NETWORK entries, so keeping every URL verbatim let an app that fetches large `data:` /
    // `blob:`-sized URLs pin CAP_NETWORK × their size (1000 × 1 MB = 1 GB). A longer URL is cut
    // with a length marker; matching and IPC parsing always see the full URL.
    var MAX_LOGGED_URL = 2048;
    function loggedUrl(url) {
        if (typeof url !== 'string' || url.length <= MAX_LOGGED_URL || isIpcUrl(url)) return url;
        var head = truncText(url, MAX_LOGGED_URL);
        return head + '…[+' + (url.length - head.length) + ' chars]';
    }
    // Victauri's own IPC (plugin:victauri|*). Decided from the parsed IPC command path — NOT a
    // substring anywhere in the URL, which let any page request containing
    // "plugin%3Avictauri%7C" in a query string escape route rules and network logging.
    function isVictauriInternalUrl(url) {
        var p = ipcCommandPath(url);
        if (p === null) return false;
        try { p = DECODE_URI_COMPONENT(p); } catch (e) {}
        return REFLECT_APPLY(STR_INDEXOF, p, ['plugin:victauri|']) === 0;
    }

    // ── IPC transport ────────────────────────────────────────────────────────
    //
    // Tauri 2 has two IPC transports (tauri `scripts/ipc-protocol.js`):
    //  - the custom-protocol `fetch()` above (desktop default), which the fetch interceptor logs;
    //  - `window.ipc.postMessage` (wry), used ALWAYS on Android, and on desktop for the rest of
    //    the page's life after ONE custom-protocol fetch fails (blocked scheme, CSP, a service
    //    worker): Tauri warns 'IPC custom protocol failed, Tauri will now use the postMessage
    //    interface instead' and sets `customProtocolIpcFailed` for good.
    // `window.ipc` and every `__TAURI_INTERNALS__` method are frozen / non-configurable, so the
    // postMessage path cannot be wrapped. Victauri observes it at two points it CAN reach:
    //  - the message serialization: Tauri's processIpcMessage calls the global
    //    `JSON.stringify(message, replacer)` on `{cmd, callback, error, payload, options,
    //    __TAURI_INVOKE_KEY__}` right before `window.ipc.postMessage(data)` — the request;
    //  - Tauri's callback table (`__TAURI_INTERNALS__.callbacks`, a plain Map): the response is
    //    delivered by `runCallback(id, data)`, which calls the function stored under the call's
    //    callback (ok) or error id, so the two entries are wrapped for that one call.
    // Captured calls go into networkLog (so every IPC reader sees them) tagged
    // `transport: 'post_message'`. The invoke key is never retained.
    var POST_MESSAGE_IPC_PREFIX = 'postmessage-ipc://localhost/';
    // The installed `JSON.stringify` observer (null until installed). If page code replaces it,
    // postMessage IPC goes unseen — the status must say so, never "full" (red team F2).
    var STRINGIFY_HOOK = null;
    function postMessageHookIntact() {
        try { return STRINGIFY_HOOK !== null && window.JSON.stringify === STRINGIFY_HOOK; } catch (e) { return false; }
    }
    var FALLBACK_WARNING_PREFIX = 'IPC custom protocol failed';
    var ipcCapture = {
        fetch_seen: false,          // any custom-protocol IPC fetch (incl. Victauri's own)
        fetch_calls: 0,             // app IPC calls logged from fetch
        post_message_seen: false,   // any postMessage IPC observed (incl. Victauri's own)
        post_message_calls: 0,      // app IPC calls logged from postMessage
        post_message_responses: 0,  // of those, responses captured
        responses_hooked: null,     // could the callback table be reached (null = not tried)
        fallback_detected_via: null,
        fallback_at: null,
    };
    function noteIpcFallback(via) {
        if (ipcCapture.fallback_detected_via === null) {
            ipcCapture.fallback_detected_via = via;
            ipcCapture.fallback_at = Date.now();
        }
    }
    // The (still URL-encoded) IPC command path of a network-log entry, or null when the entry
    // is not an IPC call. A fetch Tauri retried over postMessage is not a call of its own (the
    // postMessage entry is), so it reads as plain (failed) network traffic.
    function ipcEntryPath(n) {
        if (n.transport === 'post_message') return typeof n.ipc_path === 'string' ? n.ipc_path : null;
        if (n.superseded_by_post_message === true) return null;
        return ipcCommandPath(n.url);
    }
    // Is this entry part of the IPC log (incl. a superseded fetch attempt)?
    function isIpcEntry(n) {
        return n.transport === 'post_message' || isIpcUrl(n.url);
    }
    function flushAllIpcWaiters() {
        for (var w = ipcWaiters.length - 1; w >= 0; w--) {
            try { ipcWaiters[w](); } catch (e) {}
        }
        ipcWaiters.length = 0;
    }
    // A header value from a fetch `init.headers` (only a genuine `Headers`, as Tauri passes).
    function initHeader(init, name) {
        if (!HEADERS_GET || init === undefined || init === null) return null;
        try {
            var h = init.headers;
            if (h === undefined || h === null) return null;
            return REFLECT_APPLY(HEADERS_GET, h, [name]);
        } catch (e) { return null; }
    }
    // Tauri's callback table, or null (brand-checked: only a genuine Map).
    function tauriCallbackMap() {
        try {
            var internals = window.__TAURI_INTERNALS__;
            var m = internals ? internals.callbacks : null;
            if (m === null || m === undefined || typeof m !== 'object') return null;
            REFLECT_APPLY(MAP_HAS, m, [0]); // throws for anything but a real Map
            return m;
        } catch (e) { return null; }
    }
    function tauriPatternIsIsolation() {
        try {
            var p = window.__TAURI_INTERNALS__.__TAURI_PATTERN__;
            return !!p && p.pattern === 'isolation';
        } catch (e) { return false; }
    }
    // A detached, size-bounded copy of an IPC response value (as the fetch path stores it).
    function capturedIpcValue(v) {
        if (v === undefined || v === null) return null;
        try {
            if (ARRAY_BUFFER_CTOR && v instanceof ARRAY_BUFFER_CTOR) return bodyOmitted(v.byteLength, 'application/octet-stream');
            if (IS_ARRAY_BUFFER_VIEW && IS_ARRAY_BUFFER_VIEW(v)) return bodyOmitted(v.byteLength, 'application/octet-stream');
        } catch (e) {}
        var t = typeof v;
        if (t === 'string') return v.length > MAX_IPC_BODY ? bodyOmitted(v.length, null) : v;
        if (t === 'number' || t === 'boolean') return v;
        if (t !== 'object') return '[body omitted: ' + t + ']';
        var s;
        try { s = PRISTINE_STRINGIFY(v); } catch (e) { return '[body omitted: not serializable]'; }
        if (s === undefined) return null;
        if (s.length > MAX_IPC_BODY) return bodyOmitted(s.length, null);
        try { return PRISTINE_PARSE(s); } catch (e) { return null; }
    }
    function settlePostMessageIpc(entry, isError, data) {
        if (entry.status !== 'pending') return;
        entry.status = 'ok'; // the response was delivered; the command outcome is ipc_response
        entry.ipc_response = isError ? 'error' : 'ok';
        entry.duration_ms = Date.now() - entry.timestamp;
        ipcCapture.post_message_responses++;
        if (window.__VICTAURI__._captureIpcBodies !== false) entry.response_body = capturedIpcValue(data);
        flushAllIpcWaiters();
    }
    // Wrap the function Tauri stored under `id` so delivering the response also settles `entry`.
    // The original is always called, with the original `this` and arguments.
    function wrapTauriCallback(map, id, entry, isError) {
        var orig;
        try { orig = REFLECT_APPLY(MAP_GET, map, [id]); } catch (e) { return false; }
        if (typeof orig !== 'function') return false;
        var wrapped = function() {
            try { settlePostMessageIpc(entry, isError, arguments[0]); } catch (e) {}
            return REFLECT_APPLY(orig, this, arguments);
        };
        try { REFLECT_APPLY(MAP_SET, map, [id, wrapped]); return true; } catch (e) { return false; }
    }
    // The logged custom-protocol fetch that carried `callbackId`, or null.
    function fetchCallFor(callbackId) {
        for (var i = networkLog.length - 1; i >= 0; i--) {
            var n = networkLog[i];
            if (n.transport !== 'post_message' && n.ipc_callback === callbackId) return n;
        }
        return null;
    }
    // Tauri retries a call over postMessage (same callback ids) only after its fetch FAILED:
    // the fetch rejected (logged 'error' / 'blocked'). A fetch still in flight or answered — a
    // route mock included: a mock Tauri could not read is never served (see the fetch
    // wrapper), so Tauri never retries a mocked call — is not retried, so a postMessage message
    // reusing its ids is a page-staged look-alike: it must neither hide the real call nor be
    // logged (red team F1: page script could rename or erase a real call this way).
    function fetchWasRetriable(n) {
        return n.status === 'error' || n.status === 'blocked' || n.blocked === true;
    }
    function postMessageCallFor(callbackId) {
        for (var i = networkLog.length - 1; i >= 0; i--) {
            var n = networkLog[i];
            if (n.transport === 'post_message' && n.ipc_callback === callbackId) return n;
        }
        return null;
    }
    // Called (after serialization) for every `JSON.stringify(value, <function>)`. `out` is the
    // serialized string. Only Tauri's IPC message shape is acted on.
    function observePostMessageIpc(msg, out) {
        if (!hasOwn(msg, '__TAURI_INVOKE_KEY__')) return;
        var cmd = msg.cmd, cb = msg.callback, er = msg.error;
        if (typeof cmd !== 'string' || typeof cb !== 'number' || typeof er !== 'number') return;
        var map = tauriCallbackMap();
        // A real invoke registers both callbacks just before serializing its message; a
        // look-alike object page script serializes (to plant a fake call) has no such pair.
        if (map && (!REFLECT_APPLY(MAP_HAS, map, [cb]) || !REFLECT_APPLY(MAP_HAS, map, [er]))) return;
        var logged = postMessageCallFor(cb);
        if (logged !== null) {
            // The same ids serialized again. Tauri re-serializes its own message unchanged, so a
            // DIFFERENT command under the same ids means page script staged one of the two (it
            // can reach the callback table) — and which one came first proves nothing. Keep the
            // entry, but say both names were claimed for it.
            var again = ENCODE_URI_COMPONENT(cmd);
            if (again !== logged.ipc_path) {
                logged.ids_contested = true;
                var names = logged.contested_commands || [safeDecode(logged.ipc_path)];
                if (names.length < 8) names[names.length] = cmd;
                logged.contested_commands = names;
            }
            return;
        }
        var prior = fetchCallFor(cb);
        if (prior !== null && !fetchWasRetriable(prior)) return; // look-alike of a live/answered fetch
        var blocked = false;
        try { var o = msg.options; blocked = !!o && o.customProtocolIpcBlocked === true; } catch (e) {}
        ipcCapture.post_message_seen = true;
        ipcCapture.responses_hooked = !!map;
        if (prior !== null) {
            // Tauri's genuine retry of a failed fetch: the postMessage entry is the call.
            prior.superseded_by_post_message = true;
            if (blocked) noteIpcFallback('custom_protocol_blocked');
        }
        if (cmd.indexOf('plugin:victauri|') === 0) return; // Victauri's own plumbing

        var path = ENCODE_URI_COMPONENT(cmd);
        var entry = nullProto({
            id: ++networkCounter, method: 'POST', url: POST_MESSAGE_IPC_PREFIX + path,
            timestamp: Date.now(), status: 'pending', duration_ms: null,
            transport: 'post_message', ipc_path: path, ipc_callback: cb,
        });
        // Args, from the message as serialized (Tauri's replacer applied). Parsed only when
        // small; the parsed message (which holds the invoke key) is dropped right here.
        var payloadJson;
        var parsed = null;
        if (out.length <= MAX_IPC_BODY * 2) {
            try { parsed = PRISTINE_PARSE(out); } catch (e) { parsed = null; }
        }
        var payload = (parsed && typeof parsed === 'object') ? parsed.payload : undefined;
        parsed = null;
        if (payload !== undefined) {
            try { payloadJson = PRISTINE_STRINGIFY(payload); } catch (e) { payloadJson = undefined; }
        }
        entry.arg_size_bytes = payloadJson !== undefined ? ipcArgSize(payloadJson) : (payload === undefined ? out.length : 1);
        if (window.__VICTAURI__._captureIpcBodies !== false) {
            if (tauriPatternIsIsolation()) entry.request_args = '[encrypted: Tauri isolation pattern]';
            else if (payload === undefined) entry.request_args = bodyOmitted(out.length, null);
            else if (payloadJson !== undefined && payloadJson.length > MAX_IPC_BODY) entry.request_args = bodyOmitted(payloadJson.length, null);
            else entry.request_args = payload;
        }
        if (map) {
            var okHooked = wrapTauriCallback(map, cb, entry, false);
            var errHooked = wrapTauriCallback(map, er, entry, true);
            if (!okHooked || !errHooked) ipcCapture.responses_hooked = false;
        }
        if (!map || ipcCapture.responses_hooked === false) {
            // Without the callback table the response is invisible: never leave the call
            // 'pending' forever (that would wedge ipc_idle and read as a stuck call).
            entry.status = 'unobserved';
            entry.response_capture = false;
        }
        ipcCapture.post_message_calls++;
        appendCapped(networkLog, entry, CAP_NETWORK);
    }
    function ipcCaptureStatus() {
        var c = ipcCapture;
        var transport;
        if (c.fallback_detected_via !== null) transport = 'post_message_fallback';
        else if (c.post_message_seen) transport = 'post_message';
        else if (c.fetch_seen) transport = 'fetch';
        else transport = 'unknown';
        var capture;
        if (transport === 'fetch') capture = 'full';
        else if (transport === 'unknown') capture = 'unknown';
        else if (!c.post_message_seen) capture = 'none';
        else capture = c.responses_hooked === false ? 'requests_only' : 'full';
        var hookIntact = postMessageHookIntact();
        if (!hookIntact && transport !== 'fetch' && transport !== 'unknown') capture = 'none';
        var note;
        if (transport === 'unknown') {
            note = 'No Tauri IPC observed in this page yet.';
        } else if (transport === 'fetch') {
            note = 'IPC runs over Tauri\'s custom-protocol fetch: command, args, status, duration and response body are captured.';
        } else {
            note = (transport === 'post_message_fallback'
                ? 'Tauri switched this page to its postMessage IPC fallback after a custom-protocol fetch failed (blocked scheme, CSP or a service worker); it stays there until the page reloads. '
                : 'Tauri uses postMessage IPC in this page (always on Android). ');
            if (capture === 'full') {
                note += 'Victauri captures these calls from the serialized IPC message and Tauri\'s response callback (command, args, status, duration, response body), tagged transport:"post_message". Page script can stage such a call (Tauri\'s callback functions are page-callable), so they are unproven: never replayed by `recording replay`, and ghost detection does not count them as proof that a handler exists.';
            } else if (capture === 'requests_only') {
                note += 'Tauri\'s callback table is not reachable, so only REQUESTS are captured (command, args); their outcome is logged as status "unknown".';
            } else {
                note += 'Victauri could NOT observe the postMessage traffic: the IPC log, ghost detection, command_catalog, coverage and command_timings are BLIND to calls made since the switch.';
            }
        }
        if (!hookIntact && STRINGIFY_HOOK !== null) {
            note += ' Page code has replaced JSON.stringify, removing the observer Victauri uses for postMessage IPC: '
                + (transport === 'fetch' || transport === 'unknown'
                    ? 'a switch to postMessage IPC would go unseen.'
                    : 'calls since then are not captured.');
        }
        return {
            transport: transport,
            capture: capture,
            post_message_hook: STRINGIFY_HOOK === null ? 'missing' : (hookIntact ? 'installed' : 'replaced'),
            fetch_calls: c.fetch_calls,
            post_message_calls: c.post_message_calls,
            post_message_responses: c.post_message_responses,
            fallback_detected_via: c.fallback_detected_via,
            fallback_at: c.fallback_at,
            note: note,
        };
    }

    var navigationLog = [];
    var dialogLog = [];
    var interactionLog = [];
    var CAP_INTERACTION = 500;
    var ipcWaiters = [];
    var longTasks = [];
    var listenerCount = 0;

    // ── Network route rules (Phase 1: interception / mock / block / delay) ──
    var routeRules = [];
    var routeCounter = 0;
    var routeMatchLog = [];
    var CAP_ROUTE_MATCHES = 200;

    // Glob match: `*` (any run of characters) is the only wildcard; every other character matches
    // itself (R5-JS4). A greedy scan, linear in the URL: the regex it replaces took quadratic time
    // on a long URL, about two minutes of frozen main thread per 1 MB `data:` fetch.
    function globHits(glob, url) {
        var parts = glob.split('*');
        if (parts.length === 1) return url === glob;
        var first = parts[0], last = parts[parts.length - 1];
        var end = url.length - last.length;
        if (end < first.length) return false;
        if (REFLECT_APPLY(STR_SUBSTRING, url, [0, first.length]) !== first) return false;
        if (REFLECT_APPLY(STR_SUBSTRING, url, [end]) !== last) return false;
        var pos = first.length;
        for (var i = 1; i < parts.length - 1; i++) {
            var at = REFLECT_APPLY(STR_INDEXOF, url, [parts[i], pos]);
            if (at === -1 || at + parts[i].length > end) return false;
            pos = at + parts[i].length;
        }
        return true;
    }

    // The element actually hit at (x, y), descending into open shadow roots (which
    // `elementFromPoint` retargets to their host). Closed shadow roots stay at the host.
    function deepElementFromPoint(doc, x, y) {
        var hit = doc.elementFromPoint(x, y);
        for (var depth = 0; hit && hit.shadowRoot && depth < 32; depth++) {
            var inner = hit.shadowRoot.elementFromPoint(x, y);
            if (!inner || inner === hit) break;
            hit = inner;
        }
        return hit;
    }
    // `a` contains `b` in the flat tree (what is rendered and hit-tested), or is `b`: across
    // shadow boundaries (host), and from slotted light-DOM content to the <slot> it renders in
    // (assignedSlot). Without the slot step, a click on `<button><slot>` whose centre lands on
    // the slotted text read as "covered by" that text (R5-JS3).
    function composedContains(a, b) {
        for (var n = b; n; n = n.assignedSlot || n.parentNode || n.host) {
            if (n === a) return true;
        }
        return false;
    }

    // Find the first active route rule matching url+method, or null.
    // Never matches Victauri's own internal IPC traffic.
    // A route delay as setTimeout can honour it. Timers hold a signed 32-bit delay: anything
    // above 2^31-1 ms (or Infinity) fires almost at once, so a "delay forever" rule silently
    // became no delay (G-12). Clamp to the maximum; NaN / negative / non-numbers mean none.
    var MAX_TIMER_DELAY_MS = 2147483647;
    function clampTimerDelay(ms) {
        if (typeof ms !== 'number' || !(ms > 0)) return 0;
        return ms > MAX_TIMER_DELAY_MS ? MAX_TIMER_DELAY_MS : ms;
    }

    // The forms a request URL is matched in (R5B-ROUTEURL1). `fetch('/api/x')` hands the bridge
    // the relative string while a `Request` or `URL` object yields the absolute URL, so a rule
    // matched against only one of them hit one spelling of a request and silently missed the
    // other. A rule is tested against: the absolute URL (resolved against the document base URL,
    // as fetch/XHR resolve it), the path + query + fragment when same-origin, and the string as
    // the app passed it. The URL built-ins are the ones captured at init.
    // Resolution is skipped for schemes that have no relative or same-origin spelling (a large
    // data: URL is common, and parsing it for nothing is pure cost) and for very long URLs.
    var ROUTE_RESOLVE_MAX = 8192;
    var OPAQUE_URL_RE = /^\s*(data|blob|javascript|about):/i;
    function routeUrlForms(url) {
        var forms = [url];
        if (!URL_CTOR || !URL_HREF_GET) return forms;
        if (url.length > ROUTE_RESOLVE_MAX || OPAQUE_URL_RE.test(url)) return forms;
        try {
            var base = BASE_URI_GET ? REFLECT_APPLY(BASE_URI_GET, document, []) : window.location.href;
            var u = new URL_CTOR(url, base);
            var abs = REFLECT_APPLY(URL_HREF_GET, u, []);
            if (abs !== url) forms[forms.length] = abs;
            var loc = new URL_CTOR(window.location.href);
            var sameOrigin = REFLECT_APPLY(URL_PROTOCOL_GET, u, []) === REFLECT_APPLY(URL_PROTOCOL_GET, loc, [])
                && REFLECT_APPLY(URL_HOST_GET, u, []) === REFLECT_APPLY(URL_HOST_GET, loc, []);
            if (sameOrigin) {
                var rel = REFLECT_APPLY(URL_PATHNAME_GET, u, []) + REFLECT_APPLY(URL_SEARCH_GET, u, [])
                    + REFLECT_APPLY(URL_HASH_GET, u, []);
                if (rel !== url) forms[forms.length] = rel;
            }
        } catch (e) {}
        return forms;
    }

    function routeRuleHits(r, url) {
        try {
            if (r.match_type === 'exact') return url === r.pattern;
            if (r.match_type === 'regex') {
                // A backtracking pattern over a huge opaque URL can freeze the page; such a URL is
                // not what a regex rule is written for (substring and glob still see it).
                if (url.length > ROUTE_RESOLVE_MAX && OPAQUE_URL_RE.test(url)) return false;
                return REFLECT_APPLY(REGEXP_TEST, new REGEXP_CTOR(r.pattern), [url]);
            }
            if (r.match_type === 'glob') return globHits(r.pattern, url);
            return REFLECT_APPLY(STR_INDEXOF, url, [r.pattern]) !== -1; // substring (default)
        } catch (e) { return false; }
    }

    // A regex that can hang the page, told apart before it runs. JavaScript has no regex timeout,
    // and a rule's regex runs on the main thread against every request URL. What backtracks
    // exponentially is a repetition with more than one way to divide the same text between its
    // iterations: `(a+)+`, `(a|aa)+`, `(a+b*)+`. So the pattern is parsed down to its atoms, and
    // each repeated group is refused if two neighbouring atoms, one of them of variable length, can
    // take the same character (the last and first atoms of an iteration are neighbours too), or if
    // two of its branches can start alike. `(/\w+)+` is not: `/` cannot be taken by `\w+`.
    //
    // This is star height, not a proof. It does not catch what is polynomial rather than exponential
    // outside a repeated group (`.*.*.*.*x`), and it refuses a few patterns that are harmless
    // (`(ab|ac)+`, whose branches start alike).
    function parseRegexShape(src) {
        var i = 0, n = src.length;
        function skipTo(c) { while (i < n && src[i] !== c) i++; i++; }
        function quantifier(atom) {
            var c = src[i], min, max;
            if (c === '*') { min = 0; max = Infinity; i++; }
            else if (c === '+') { min = 1; max = Infinity; i++; }
            else if (c === '?') { min = 0; max = 1; i++; }
            else if (c === '{') {
                // Anything but `{n}`, `{n,}`, `{n,m}` is a literal brace, already taken as an atom.
                var j = i + 1, a = '', b = '', comma = false;
                while (src[j] >= '0' && src[j] <= '9') a += src[j++];
                if (src[j] === ',') { comma = true; j++; }
                while (src[j] >= '0' && src[j] <= '9') b += src[j++];
                if (a === '' || src[j] !== '}') return;
                min = +a;
                max = !comma ? min : (b === '' ? Infinity : +b);
                i = j + 1;
            } else return;
            if (src[i] === '?') i++; // lazy
            atom.min = min;
            atom.max = max;
        }
        function atom() {
            var start = i, c = src[i++], made = { min: 1, max: 1, start: start };
            if (c === '(') {
                if (src[i] === '?') {
                    var d = src[i + 1];
                    if (d === '=' || d === '!') { made.zero = true; i += 2; }
                    else if (d === '<' && (src[i + 2] === '=' || src[i + 2] === '!')) { made.zero = true; i += 3; }
                    else if (d === '<') skipTo('>');
                    else skipTo(':');
                }
                made.alts = alternatives();
                i++; // the `)`
                return made;
            }
            if (c === '[') {
                while (i < n && src[i] !== ']') { if (src[i] === '\\') i++; i++; }
                i++;
            } else if (c === '^' || c === '$') {
                made.zero = true;
            } else if (c === '\\') {
                var e = src[i++];
                if (e === 'b' || e === 'B') made.zero = true;
                else if (e >= '1' && e <= '9') {
                    while (src[i] >= '0' && src[i] <= '9') i++;
                    made.any = true; // a backreference: any text, of any length
                } else if (e === 'k' && src[i] === '<') { skipTo('>'); made.any = true; }
                else if (e === 'x') i += 2;
                else if (e === 'u') { if (src[i] === '{') skipTo('}'); else i += 4; }
                else if (e === 'c') i += 1;
            }
            made.text = REFLECT_APPLY(STR_SUBSTRING, src, [start, i]);
            return made;
        }
        function alternatives() {
            var alts = [[]];
            while (i < n && src[i] !== ')') {
                if (src[i] === '|') { i++; alts[alts.length] = []; continue; }
                var made = atom();
                quantifier(made);
                made.end = i;
                var seq = alts[alts.length - 1];
                seq[seq.length] = made;
            }
            return alts;
        }
        return alternatives();
    }
    function regexHangRisk(src) {
        var alts = parseRegexShape(src);
        // The characters tried: ASCII, and every other character the pattern names.
        var probes = [];
        for (var c = 0; c < 128; c++) probes[probes.length] = String.fromCharCode(c);
        for (var u = 0; u < src.length; u++) { if (src.charCodeAt(u) > 127) probes[probes.length] = src[u]; }
        probes[probes.length] = '\u00e9';
        probes[probes.length] = '\u2028';
        function chars(a) {
            if (a.chars) return a.chars;
            var out = [], k;
            if (a.alts) {
                for (k = 0; k < probes.length; k++) out[k] = false;
                a.alts.forEach(function(seq) { seq.forEach(function(sub) {
                    if (sub.zero) return;
                    var inner = chars(sub);
                    for (var m = 0; m < probes.length; m++) out[m] = out[m] || inner[m];
                }); });
            } else if (a.any) {
                for (k = 0; k < probes.length; k++) out[k] = true;
            } else {
                try {
                    var re = new REGEXP_CTOR('^(?:' + a.text + ')$');
                    for (k = 0; k < probes.length; k++) out[k] = REFLECT_APPLY(REGEXP_TEST, re, [probes[k]]);
                } catch (e) {
                    for (k = 0; k < probes.length; k++) out[k] = true;
                }
            }
            a.chars = out;
            return out;
        }
        function overlap(a, b) {
            var x = chars(a), y = chars(b);
            for (var k = 0; k < x.length; k++) { if (x[k] && y[k]) return true; }
            return false;
        }
        // An iteration's real atoms: no zero-width ones, and a plain group is its contents.
        function flat(seq) {
            var out = [];
            seq.forEach(function(a) {
                if (a.zero) return;
                if (a.alts && a.alts.length === 1 && a.min === 1 && a.max === 1) out = out.concat(flat(a.alts[0]));
                else out[out.length] = a;
            });
            return out;
        }
        function varies(a) {
            if (a.any || a.min !== a.max) return true;
            if (!a.alts) return false;
            return a.alts.some(function(seq) {
                var f = flat(seq);
                return (a.alts.length > 1 && f.length !== 1) || f.some(varies);
            });
        }
        function nullable(a) {
            if (a.any || a.min === 0) return true;
            return !!a.alts && a.alts.some(function(seq) { return flat(seq).every(nullable); });
        }
        // What can start (or, reversed, end) an iteration: up to the first atom it cannot skip.
        function reach(seq) {
            var out = [];
            for (var k = 0; k < seq.length; k++) {
                out[out.length] = seq[k];
                if (!nullable(seq[k])) break;
            }
            return out;
        }
        function clash(a, b) { return (varies(a) || varies(b)) && overlap(a, b); }
        function ambiguous(group) {
            var branches = group.alts.map(flat);
            var heads = branches.map(reach);
            var tails = branches.map(function(seq) { return reach(seq.slice().reverse()); });
            for (var b = 0; b < branches.length; b++) {
                if (branches[b].every(nullable)) return true;
                for (var other = b + 1; other < branches.length; other++) {
                    if (heads[b].some(function(x) { return heads[other].some(function(y) { return overlap(x, y); }); })) return true;
                }
                for (var k = 0; k < branches[b].length; k++) {
                    for (var l = k + 1; l < branches[b].length; l++) {
                        if (clash(branches[b][k], branches[b][l])) return true;
                        if (!nullable(branches[b][l])) break;
                    }
                }
                for (var next = 0; next < branches.length; next++) {
                    if (tails[b].some(function(x) { return heads[next].some(function(y) { return clash(x, y); }); })) return true;
                }
            }
            return false;
        }
        function visit(list) {
            for (var q = 0; q < list.length; q++) {
                for (var r = 0; r < list[q].length; r++) {
                    var a = list[q][r];
                    if (!a.alts) continue;
                    if (!a.zero && a.max >= 2 && ambiguous(a)) return REFLECT_APPLY(STR_SUBSTRING, src, [a.start, a.end]);
                    var inner = visit(a.alts);
                    if (inner) return inner;
                }
            }
            return null;
        }
        return visit(alts);
    }

    function matchRoute(url, method) {
        if (!routeRules.length) return null;
        if (isVictauriInternalUrl(url)) return null;
        var m = (method || 'GET').toUpperCase();
        var forms = routeUrlForms(url);
        for (var i = 0; i < routeRules.length; i++) {
            var r = routeRules[i];
            if (r.times && r.triggered >= r.times) continue;
            if (r.method && r.method.toUpperCase() !== m) continue;
            for (var f = 0; f < forms.length; f++) {
                if (routeRuleHits(r, forms[f])) return r;
            }
        }
        return null;
    }

    // What a rule does to Tauri IPC, when it can match an IPC URL (a broad pattern such as `*`
    // or `localhost` does too). Tauri's fetch transport runs through the page's `fetch`, so a
    // rule really changes what the app's `invoke` gets — say exactly how.
    // Can this rule match a Tauri IPC URL? Either it names the IPC host (`ipc.localhost/greet`,
    // `ipc://localhost/…`, a regex such as `ipc\.localhost`) — the usual, command-specific case —
    // or it is broad enough to match any IPC URL (`*`, `localhost`), found by probing.
    function routeMatchesIpc(r) {
        if (r.method && ('' + r.method).toUpperCase() !== 'POST') return false; // Tauri IPC is POST
        var pat = ('' + r.pattern).toLowerCase();
        if (pat.indexOf('ipc') !== -1 && pat.indexOf('localhost') !== -1) return true;
        var probes = [];
        for (var pi = 0; pi < IPC_PREFIXES.length; pi++) probes.push(IPC_PREFIXES[pi] + '__victauri_probe__');
        for (var i = 0; i < probes.length; i++) {
            var forms = routeUrlForms(probes[i]);
            for (var f = 0; f < forms.length; f++) {
                try { if (routeRuleHits(r, forms[f])) return true; } catch (e) {}
            }
        }
        return false;
    }
    // The Response a `fulfill` rule serves, and its body text (null for a null-body status).
    // Throws whatever the Response constructor throws (an invalid header name or value, a
    // status text that is not a ByteString, …) — `addRoute` builds it once to refuse such a rule.
    function buildMockResponse(route) {
        var status = route.status;
        if (typeof status !== 'number' || status !== Math.floor(status) || status < 200 || status > 599) {
            throw new RangeError('victauri: route #' + route.id + ' fulfill status must be an integer in 200-599, got ' + status);
        }
        var bodyStr = mockBody(route);
        var Ctor = RESPONSE_CTOR || Response;
        return { resp: new Ctor(bodyStr, { status: status, statusText: route.status_text, headers: mockHeaders(route) }), body: bodyStr };
    }
    // A mock's body text, null for a null-body status (the Response constructor throws if one
    // carries a body).
    function mockBody(route) {
        var nullBody = route.status === 204 || route.status === 205 || route.status === 304;
        return nullBody ? null : ((typeof route.body === 'string') ? route.body : PRISTINE_STRINGIFY(route.body));
    }
    function mockHeaders(route) {
        var hdrs = OBJ_CREATE(null);
        hdrs['content-type'] = route.content_type;
        // Lower-cased: `Content-Type` next to the default `content-type` became a joined, doubled header.
        for (var k in route.headers) { if (hasOwn(route.headers, k)) hdrs[REFLECT_APPLY(STR_TO_LOWER, k, [])] = route.headers[k]; }
        return hdrs;
    }
    // The part of a response's Content-Type that Tauri's ipc-protocol.js switches on: the
    // header as `Headers` gives it (trimmed, repeated names joined with ", "), up to the first comma.
    function tauriContentTypeHead(resp) {
        var ct = respHeader(resp, 'content-type') || '';
        var comma = REFLECT_APPLY(STR_INDEXOF, ct, [',']);
        return comma < 0 ? ct : REFLECT_APPLY(STR_SUBSTRING, ct, [0, comma]);
    }
    // Why Tauri could not read this rule's mock as an IPC response (its `response.json()` would
    // throw, so Tauri would run the REAL command over postMessage), or null.
    // Whether this realm has a `Response` constructor to build mocks with (every webview does).
    function canBuildResponses() {
        return RESPONSE_CTOR !== null || typeof Response === 'function';
    }
    function ipcMockUnparsable(r) {
        if (!canBuildResponses()) return null; // nothing to judge; a fulfill cannot be served here either
        var built;
        try { built = buildMockResponse(r); } catch (e) { return 'the mock response cannot be built (' + e + ').'; }
        if (tauriContentTypeHead(built.resp) !== 'application/json') return null; // text() / arrayBuffer() never throw
        if (built.body === null) return 'status ' + r.status + ' carries no body, which Tauri reads as JSON.';
        try { PRISTINE_PARSE(built.body); return null; } catch (e) {
            return 'its body is not valid JSON (' + (built.body === '' ? 'empty' : 'e.g. an unquoted string') + '), and Tauri reads it as JSON.';
        }
    }
    function routeIpcNote(r) {
        var scope = ' Applies to the fetch transport only (desktop, until a fallback): IPC sent over'
            + ' postMessage (Android, or after a fallback) is not affected.';
        if (r.action === 'fulfill') {
            return 'This rule ANSWERS matching Tauri IPC calls in the page: the app\'s invoke gets'
                + ' this mock and the Rust handler never runs. invoke resolves only when the headers'
                + ' include "Tauri-Response": "ok" (otherwise it rejects with the body). Tauri takes'
                + ' the body as JSON for application/json, text for text/plain, else an ArrayBuffer'
                + ' (a mock it could not parse is refused). Mocked calls are tagged `mocked` in the'
                + ' IPC log and left out of ghost detection, command_catalog, coverage and timings.' + scope;
        }
        if (r.action === 'block') {
            return 'Blocking a Tauri IPC fetch does NOT fail the call: Tauri retries it over'
                + ' postMessage (the backend still runs it) and keeps the page on postMessage IPC'
                + ' until it reloads. To make a command fail, use `fulfill` without a'
                + ' "Tauri-Response": "ok" header.';
        }
        return 'Matching Tauri IPC calls are held back before reaching the backend; their logged'
            + ' duration includes the delay (`route_delay_ms`), so command_timings leaves them out.' + scope;
    }

    function recordRouteMatch(rule, url, method) {
        rule.triggered = (rule.triggered || 0) + 1;
        var m = {
            rule_id: rule.id, action: rule.action, url: loggedUrl(url),
            method: (method || 'GET').toUpperCase(), timestamp: Date.now(),
            trigger_count: rule.triggered,
        };
        appendCapped(routeMatchLog, m, CAP_ROUTE_MATCHES);
    }

    // `:disabled` covers a control inside `<fieldset disabled>`, which `el.disabled` does not (click
    // is then a spec no-op that reported ok). `=== true` because a form's `disabled` can be a
    // clobbering named control.
    function isDisabledControl(el) {
        if (el.disabled === true) return true;
        try {
            if (REFLECT_APPLY(EL_MATCHES, el, [':disabled'])) return true;
            // A text query returns the deepest match, often a span inside the control. A fieldset
            // is left out: plain content in a disabled fieldset is not a control.
            return !!REFLECT_APPLY(EL_CLOSEST, el, ['button:disabled, select:disabled, optgroup:disabled, option:disabled']);
        } catch (e) { return false; }
    }
    function isAriaDisabled(el) {
        // aria-disabled applies to the element's descendants too.
        try { return !!REFLECT_APPLY(EL_CLOSEST, el, ['[aria-disabled="true"]']); } catch (e) { return false; }
    }
    function isElEnabled(el) { return !isDisabledControl(el) && !isAriaDisabled(el); }
    // A `<label>` (or something inside one) for `el`: clicking it operates `el`, as a user clicking
    // a styled checkbox's label does.
    function labelOperates(hit, el) {
        for (var n = hit, g = 0; n && g < 16; n = n.parentNode, g++) {
            if (n.tagName === 'LABEL') {
                try { return n.control === el; } catch (e) { return false; }
            }
        }
        return false;
    }
    function hitCovers(hit, el) {
        return !!hit && hit !== el && !composedContains(el, hit) && !labelOperates(hit, el);
    }
    // Shapes whose box says nothing about what they paint at its centre: SVG, an inline element
    // wrapped over several lines, and a box with no area around content laid out outside it.
    function paintsOffCentre(el, rect) {
        if (el.namespaceURI === 'http://www.w3.org/2000/svg') return true;
        if (rect.width === 0 || rect.height === 0) return true;
        var lines = typeof el.getClientRects === 'function' ? el.getClientRects() : null;
        return !!lines && lines.length > 1;
    }
    // A control that a click on its content operates, whatever paints on top inside it (a
    // `.stretched-link` or ripple `::after` is hit as the control).
    function isOperableControl(el) {
        var tag = el.tagName;
        if (tag === 'BUTTON' || tag === 'SUMMARY' || (tag === 'A' && elAttr(el, 'href') !== null)) return true;
        var role = elAttr(el, 'role');
        return role === 'button' || role === 'link';
    }
    // `el` (or something inside it) is in the stack at the point, under whatever is on top. Hit
    // testing honours clipping, so content clipped away below a control is not.
    function stackHolds(el, x, y) {
        var root = typeof el.getRootNode === 'function' ? el.getRootNode() : null;
        if (!root || typeof root.elementsFromPoint !== 'function') return false;
        var stack = root.elementsFromPoint(x, y);
        for (var i = 0; i < stack.length; i++) {
            if (composedContains(el, stack[i])) return true;
        }
        return false;
    }
    // Would a real click at the point reach `el`? Only the browser can say: `elementFromPoint`
    // honours clipping, `clip-path`, pseudo-elements and z-order. `strict` is for an OS click,
    // which lands on what is hit: it must be `el` or inside it.
    function pointReaches(doc, el, x, y, strict) {
        var hit = deepElementFromPoint(doc, x, y);
        if (!hit) return false;
        if (strict) return composedContains(el, hit);
        if (!hitCovers(hit, el)) return true;
        return composedContains(hit, el) && isOperableControl(hit) && stackHolds(el, x, y);
    }
    // Points along an SVG shape's outline in viewport coordinates (a stroke-only circle paints
    // nothing else). `getScreenCTM` maps to the viewport of the element's own frame.
    function outlinePoints(el, out) {
        if (typeof el.getPointAtLength !== 'function' || typeof el.getScreenCTM !== 'function') return;
        try {
            var m = el.getScreenCTM(), len = el.getTotalLength();
            if (!m || !(len > 0)) return;
            for (var i = 0; i < 16; i++) {
                var p = el.getPointAtLength(len * (i + 0.5) / 16);
                out[out.length] = { x: m.a * p.x + m.c * p.y + m.e, y: m.b * p.x + m.d * p.y + m.f };
            }
        } catch (e) {}
    }
    function lineCentres(el, out) {
        var lines = typeof el.getClientRects === 'function' ? el.getClientRects() : null;
        if (!lines || lines.length < 2) return;
        for (var i = 0; i < lines.length; i++) {
            out[out.length] = { x: lines[i].left + lines[i].width / 2, y: lines[i].top + lines[i].height / 2 };
        }
    }
    // Up to 32 descendants with a box, nearest first: what an empty inline box or a `<g>` paints.
    function boxedDescendants(el) {
        var found = [], queue = [el];
        for (var q = 0; q < queue.length && found.length < 32 && queue.length < 512; q++) {
            var kids = queue[q].children;
            for (var k = 0; kids && k < kids.length; k++) {
                var b = kids[k].getBoundingClientRect();
                if (b.width > 0 || b.height > 0) found[found.length] = kids[k];
                queue[queue.length] = kids[k];
            }
        }
        return found;
    }
    // The first real point of `el` that a click would reach: along its outline, the centre of every
    // line box, a 5x5 grid over its box, then what its descendants paint outside it.
    function sampledPoint(doc, win, el, rect, strict) {
        var vw = win.innerWidth || doc.documentElement.clientWidth;
        var vh = win.innerHeight || doc.documentElement.clientHeight;
        var at = [];
        outlinePoints(el, at);
        lineCentres(el, at);
        for (var row = 0; row < 5; row++) {
            for (var col = 0; col < 5; col++) {
                at[at.length] = { x: rect.left + rect.width * (col + 0.5) / 5, y: rect.top + rect.height * (row + 0.5) / 5 };
            }
        }
        var kids = boxedDescendants(el);
        for (var k = 0; k < kids.length; k++) {
            outlinePoints(kids[k], at);
            lineCentres(kids[k], at);
            var b = kids[k].getBoundingClientRect();
            at[at.length] = { x: b.left + b.width / 2, y: b.top + b.height / 2 };
        }
        for (var i = 0; i < at.length; i++) {
            var p = at[i];
            if (p.x >= 0 && p.y >= 0 && p.x < vw && p.y < vh && pointReaches(doc, el, p.x, p.y, strict)) return p;
        }
        return null;
    }
    // Where a real click on `el` would reach it: `{x, y}` in its own frame's viewport (its centre
    // when that works), or `{error, hint}`. `strict`: see `pointReaches`.
    function hitPoint(el, strict) {
        if (!el || !el.isConnected) return { error: 'element is detached from DOM', hint: 'RETRY_LATER' };
        if (isDisabledControl(el)) return { error: 'element is disabled (disabled attribute)', hint: 'RETRY_LATER' };
        if (isAriaDisabled(el)) return { error: 'element is disabled (aria-disabled)', hint: 'RETRY_LATER' };
        // Use the element's OWN document/window so the viewport and occlusion
        // (elementFromPoint) checks are correct for elements inside same-origin
        // iframes — getBoundingClientRect() is relative to the element's own
        // frame viewport, not the top document.
        var doc = el.ownerDocument || document;
        var win = doc.defaultView || window;
        var cs = win.getComputedStyle(el);
        if (cs.display === 'none') return { error: 'element is not visible (display: none)', hint: 'RETRY_LATER' };
        if (cs.visibility === 'hidden') return { error: 'element is not visible (visibility: hidden)', hint: 'RETRY_LATER' };
        // No opacity test: hit testing decides. An `opacity:0` input over a styled box is what a
        // user clicks (the MUI / Ant / Chakra checkbox pattern).
        var rect = el.getBoundingClientRect();
        // An empty `<a>` around an absolute or floated child has no box of its own, and is clickable.
        if (rect.width === 0 && rect.height === 0 && !boxedDescendants(el).length) return { error: 'element has zero size', hint: 'RETRY_LATER' };
        if (cs.pointerEvents === 'none') return { error: 'element has pointer-events: none', hint: 'RETRY_LATER' };
        var vw = win.innerWidth || doc.documentElement.clientWidth;
        var vh = win.innerHeight || doc.documentElement.clientHeight;
        if (rect.bottom < 0 || rect.top > vh || rect.right < 0 || rect.left > vw) {
            el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
            rect = el.getBoundingClientRect();
            if (rect.bottom < 0 || rect.top > vh || rect.right < 0 || rect.left > vw) {
                return { error: 'element is outside viewport after scroll attempt', hint: 'CHECK_INPUT' };
            }
        }
        var cx = rect.left + rect.width / 2;
        var cy = rect.top + rect.height / 2;
        // Hit-test THROUGH open shadow roots: `elementFromPoint` reports anything inside a
        // shadow tree as its host, and `Node.contains` never crosses a shadow boundary, so every
        // element in a web component (Lit, Shoelace, Ionic, ...) read as "covered by <host>".
        var topEl = deepElementFromPoint(doc, cx, cy);
        // An ancestor at the point is either really on top, or `el` is scrolled out of a scroll
        // container (the container is then what is hit). Scroll once before calling it covered.
        if (hitCovers(topEl, el) && composedContains(topEl, el) && typeof el.scrollIntoView === 'function') {
            el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
            rect = el.getBoundingClientRect();
            cx = rect.left + rect.width / 2;
            cy = rect.top + rect.height / 2;
            topEl = deepElementFromPoint(doc, cx, cy);
        }
        // Nothing at all at the centre: it is outside the viewport.
        if (strict ? pointReaches(doc, el, cx, cy, true) : (!topEl || pointReaches(doc, el, cx, cy, false))) {
            return { x: cx, y: cy };
        }
        // The centre is not on `el`. Ask the browser about points that are. An ancestor at the
        // centre says nothing (`el` may have a hole there, or be clipped to a corner), and neither
        // does anything at the centre of a shape that does not paint it. A sibling at the centre of
        // anything else is cover.
        if (!topEl || composedContains(topEl, el) || paintsOffCentre(el, rect)) {
            var found = sampledPoint(doc, win, el, rect, strict);
            if (found) return found;
        }
        var tag = topEl && topEl.tagName ? topEl.tagName.toLowerCase() : 'unknown';
        var info = tag;
        if (topEl && topEl.id) info += '#' + topEl.id;
        else if (topEl && topEl.className && typeof topEl.className === 'string') {
            var cls = topEl.className.trim().split(/\s+/)[0];
            if (cls) info += '.' + cls;
        }
        return { error: 'element is covered by ' + info + ' at (' + Math.round(cx) + ',' + Math.round(cy) + ')', hint: 'RETRY_LATER' };
    }
    function checkActionable(el) {
        var point = hitPoint(el, false);
        return point.error ? point : null;
    }

    function withAutoWait(refId, timeoutMs, actionFn) {
        return new Promise(function(resolve) {
            var deadline = Date.now() + (timeoutMs || 5000);
            function attempt() {
                // A throw from resolveRef/checkActionable (e.g. getComputedStyle on
                // an element in a torn-down frame) must resolve with an error, not
                // escape into a setTimeout and leave the caller hanging until the
                // eval timeout.
                try { attemptInner(); }
                catch (e) { resolve({ ok: false, error: 'actionability check threw: ' + (e && e.message), hint: 'CHECK_INPUT' }); }
            }
            function attemptInner() {
                var el = resolveRef(refId);
                if (!el) {
                    // A ref of another page load can never appear here: no point waiting.
                    if (Date.now() >= deadline || isForeignRef(refId)) { resolve({ ok: false, error: refNotFound(refId), hint: 'CHECK_INPUT' }); return; }
                    setTimeout(attempt, 50); return;
                }
                var check = checkActionable(el);
                if (check) {
                    if (check.hint === 'CHECK_INPUT' || Date.now() >= deadline) {
                        var msg = Date.now() >= deadline ? 'timeout (' + (timeoutMs || 5000) + 'ms): ' + check.error : check.error;
                        resolve({ ok: false, error: msg, hint: check.hint || 'RETRY_LATER' }); return;
                    }
                    setTimeout(attempt, 50); return;
                }
                try { var r = actionFn(el); resolve(r || { ok: true }); }
                catch (e) { resolve({ ok: false, error: 'action threw: ' + e.message, hint: 'CHECK_INPUT' }); }
            }
            attempt();
        });
    }

    // ── Built-ins captured at init ──────────────────────────────────────────
    //
    // Page script runs after this init script and can replace any global or prototype method
    // (`Map.prototype.set`, `window.String`, `setTimeout`, `JSON.stringify`, …). Everything the
    // bridge's security-relevant paths call is captured here, and called through bound copies
    // so no `.call` / prototype lookup happens at call time.
    var NATIVE_STRINGIFY = JSON.stringify;
    var PRISTINE_PARSE = JSON.parse;
    var OBJ_CREATE = Object.create;
    var GET_PROTO = Object.getPrototypeOf;
    var OBJECT_PROTO = Object.prototype;
    var ARRAY_PROTO = Array.prototype;
    var hasOwn = Function.prototype.call.bind(Object.prototype.hasOwnProperty);
    var ASSIGN = Object.assign;
    // A prototype-less copy of an object's own fields. Log entries, rule specs and query objects
    // are read field by field; on a plain object every ABSENT field falls through to
    // `Object.prototype`, where page script can plant `mocked`, `transport`, `role`, ...
    function nullProto(o) { return ASSIGN(OBJ_CREATE(null), o); }
    function ownCopy(o) { return (o !== null && typeof o === 'object') ? nullProto(o) : OBJ_CREATE(null); }
    var SET_TIMEOUT = window.setTimeout.bind(window);
    var CLEAR_TIMEOUT = window.clearTimeout.bind(window);
    var DECODE_URI_COMPONENT = decodeURIComponent;
    var REGEXP_CTOR = RegExp;
    var REGEXP_TEST = RegExp.prototype.test;
    var REFLECT_APPLY = Reflect.apply;
    // The bridge's own logs (network/IPC, route rules, route matches) are appended, trimmed and
    // walked without calling an `Array.prototype` method page script can replace: a hook there
    // is called with the LIVE array / entry as `this` / argument, and could rewrite a logged
    // call or a stored rule behind the copy-on-read hardening.
    var ARRAY_SHIFT = Array.prototype.shift;
    var ARRAY_SPLICE = Array.prototype.splice;
    function appendCapped(arr, item, cap) {
        arr[arr.length] = item;
        if (arr.length > cap) REFLECT_APPLY(ARRAY_SHIFT, arr, []);
    }
    function removeAt(arr, i) { REFLECT_APPLY(ARRAY_SPLICE, arr, [i, 1]); }
    var STR_INDEXOF = String.prototype.indexOf;
    var STR_SUBSTRING = String.prototype.substring;
    var STR_TO_LOWER = String.prototype.toLowerCase;
    var ARRAY_BUFFER_SLICE = ArrayBuffer.prototype.slice;
    // Native `Request` getters. They brand-check (throw for anything but a genuine Request), so
    // they tell a real Request apart from a look-alike object and read what fetch really uses —
    // an own `url` property planted on a Request instance cannot shadow them.
    var REQUEST_URL_GET = null;
    var REQUEST_METHOD_GET = null;
    try {
        if (typeof window.Request === 'function') {
            REQUEST_URL_GET = Object.getOwnPropertyDescriptor(window.Request.prototype, 'url').get;
            REQUEST_METHOD_GET = Object.getOwnPropertyDescriptor(window.Request.prototype, 'method').get;
        }
    } catch (e) { REQUEST_URL_GET = null; REQUEST_METHOD_GET = null; }
    // ECMAScript ToString — what fetch() / XMLHttpRequest.open() apply to a URL or method
    // argument (it throws for a Symbol, as they do). Not `String(v)`: page script can replace
    // `window.String`, and `String(symbol)` does not throw.
    function toStringExact(v) { return `${v}`; }
    // The URL constructor and the getters route matching reads (routeUrlForms), and the
    // document base URL getter fetch/XHR resolve a relative URL against.
    var URL_CTOR = typeof window.URL === 'function' ? window.URL : null;
    function protoGetter(proto, name) {
        try { var d = Object.getOwnPropertyDescriptor(proto, name); return d && d.get ? d.get : null; }
        catch (e) { return null; }
    }
    var URL_PROTO = URL_CTOR ? URL_CTOR.prototype : null;
    var URL_HREF_GET = URL_PROTO && protoGetter(URL_PROTO, 'href');
    var URL_PROTOCOL_GET = URL_PROTO && protoGetter(URL_PROTO, 'protocol');
    var URL_HOST_GET = URL_PROTO && protoGetter(URL_PROTO, 'host');
    var URL_PATHNAME_GET = URL_PROTO && protoGetter(URL_PROTO, 'pathname');
    var URL_SEARCH_GET = URL_PROTO && protoGetter(URL_PROTO, 'search');
    var URL_HASH_GET = URL_PROTO && protoGetter(URL_PROTO, 'hash');
    var BASE_URI_GET = typeof Node === 'function' ? protoGetter(Node.prototype, 'baseURI') : null;
    var AGENT_KEY = '__VICTAURI_AGENT_KEY__';
    // For the postMessage-fallback IPC capture (see "IPC transport" below).
    var ENCODE_URI_COMPONENT = encodeURIComponent;
    var MAP_GET = Map.prototype.get;
    var MAP_SET = Map.prototype.set;
    var MAP_HAS = Map.prototype.has;
    var MAP_DELETE = Map.prototype.delete;
    var MAP_CLEAR = Map.prototype.clear;
    var MAP_KEYS = Map.prototype.keys;
    var MAP_FOREACH = Map.prototype.forEach;
    var MAP_SIZE_GET = protoGetter(Map.prototype, 'size');
    var WEAKMAP_GET = WeakMap.prototype.get;
    var WEAKMAP_SET = WeakMap.prototype.set;
    var WEAKREF_CTOR = typeof WeakRef === 'function' ? WeakRef : null;
    var WEAKREF_DEREF = WEAKREF_CTOR ? WeakRef.prototype.deref : null;
    // A form's named controls shadow its DOM API (`<input name=children>`, `name=getAttribute`), so
    // the walkers read elements through the prototypes. Same-realm getters work on frame elements.
    var EL_PROTO = typeof Element === 'function' ? Element.prototype : null;
    var NODE_PROTO = typeof Node === 'function' ? Node.prototype : null;
    var EL_GET_ATTR = EL_PROTO && EL_PROTO.getAttribute;
    var EL_HAS_ATTR = EL_PROTO && EL_PROTO.hasAttribute;
    var EL_MATCHES = EL_PROTO && EL_PROTO.matches;
    var EL_CLOSEST = EL_PROTO && EL_PROTO.closest;
    var EL_RECT = EL_PROTO && EL_PROTO.getBoundingClientRect;
    var EL_CHILDREN_GET = EL_PROTO && protoGetter(EL_PROTO, 'children');
    var EL_TAG_GET = EL_PROTO && protoGetter(EL_PROTO, 'tagName');
    var EL_SHADOW_GET = EL_PROTO && protoGetter(EL_PROTO, 'shadowRoot');
    var NODE_TYPE_GET = NODE_PROTO && protoGetter(NODE_PROTO, 'nodeType');
    var NODE_CHILDNODES_GET = NODE_PROTO && protoGetter(NODE_PROTO, 'childNodes');
    function elAttr(el, n) { return EL_GET_ATTR ? REFLECT_APPLY(EL_GET_ATTR, el, [n]) : el.getAttribute(n); }
    function elHasAttr(el, n) { return EL_HAS_ATTR ? REFLECT_APPLY(EL_HAS_ATTR, el, [n]) : el.hasAttribute(n); }
    function elRect(el) { return EL_RECT ? REFLECT_APPLY(EL_RECT, el, []) : el.getBoundingClientRect(); }
    function elKids(el) { return EL_CHILDREN_GET ? REFLECT_APPLY(EL_CHILDREN_GET, el, []) : el.children; }
    function elTag(el) { return EL_TAG_GET ? REFLECT_APPLY(EL_TAG_GET, el, []) : el.tagName; }
    function elShadow(el) { return EL_SHADOW_GET ? REFLECT_APPLY(EL_SHADOW_GET, el, []) : el.shadowRoot; }
    function nodeKind(n) { return NODE_TYPE_GET ? REFLECT_APPLY(NODE_TYPE_GET, n, []) : n.nodeType; }
    function nodeKids(n) { return NODE_CHILDNODES_GET ? REFLECT_APPLY(NODE_CHILDNODES_GET, n, []) : n.childNodes; }
    var ARRAY_BUFFER_CTOR = typeof ArrayBuffer === 'function' ? ArrayBuffer : null;
    var IS_ARRAY_BUFFER_VIEW = (ARRAY_BUFFER_CTOR && typeof ArrayBuffer.isView === 'function') ? ArrayBuffer.isView : null;
    var HEADERS_GET = null;
    try {
        if (typeof window.Headers === 'function') HEADERS_GET = window.Headers.prototype.get;
    } catch (e) { HEADERS_GET = null; }
    // The native Response getters / methods: the IPC outcome (`Tauri-Response`), status and body
    // of a REAL response are read through these, not through `Response.prototype` / `Headers`
    // methods page script can replace to turn a "command not found" into a verified handler.
    var RESPONSE_CTOR = typeof window.Response === 'function' ? window.Response : null;
    var RESPONSE_HEADERS_GET = RESPONSE_CTOR ? protoGetter(RESPONSE_CTOR.prototype, 'headers') : null;
    var RESPONSE_STATUS_GET = RESPONSE_CTOR ? protoGetter(RESPONSE_CTOR.prototype, 'status') : null;
    var RESPONSE_STATUS_TEXT_GET = RESPONSE_CTOR ? protoGetter(RESPONSE_CTOR.prototype, 'statusText') : null;
    var RESPONSE_CLONE = RESPONSE_CTOR ? RESPONSE_CTOR.prototype.clone : null;
    var RESPONSE_TEXT = RESPONSE_CTOR ? RESPONSE_CTOR.prototype.text : null;
    // `name` of a Response's headers, read natively when the Response is a genuine one (else
    // through the object's own methods: a non-native Response has nothing better to offer).
    function respHeader(resp, name) {
        if (HEADERS_GET && RESPONSE_HEADERS_GET) {
            try { return REFLECT_APPLY(HEADERS_GET, REFLECT_APPLY(RESPONSE_HEADERS_GET, resp, []), [name]); } catch (e) {}
        }
        try { return resp.headers.get(name); } catch (e) { return null; }
    }
    // The body text of a copy of `resp` (the app keeps reading the original). Never throws:
    // a failure is a rejected promise the caller ignores.
    function respTextCopy(resp) {
        try {
            if (RESPONSE_CLONE && RESPONSE_TEXT) {
                try { return REFLECT_APPLY(RESPONSE_TEXT, REFLECT_APPLY(RESPONSE_CLONE, resp, []), []); } catch (e) {}
            }
            return resp.clone().text();
        } catch (e) { return Promise.reject(e); }
    }
    function respField(resp, getter, name) {
        if (getter) { try { return REFLECT_APPLY(getter, resp, []); } catch (e) {} }
        try { return resp[name]; } catch (e) { return undefined; }
    }

    // Page script can plant `toJSON` on Object.prototype / Array.prototype, which JSON.stringify
    // consults on EVERY object it serializes: that forged eval results (including the
    // `__victauri_type` envelope) and rewrote every log the agent read. A `toJSON` inherited
    // from those two universal prototypes is ignored; one an app defines on its own class
    // (or a built-in such as Date) is honoured as usual.
    function hasUniversalToJSON(o) {
        for (var p = o; p !== null && p !== undefined; p = GET_PROTO(p)) {
            if (hasOwn(p, 'toJSON')) return p === OBJECT_PROTO || p === ARRAY_PROTO;
        }
        return false;
    }
    function neutralReplacer(key, value) {
        // `this[key]` is the raw value before toJSON; JSON.stringify never re-applies toJSON
        // to what a replacer returns, so returning it serializes the object's own fields.
        var raw = this[key];
        if (raw !== value && raw !== null && typeof raw === 'object' && hasUniversalToJSON(raw)) return raw;
        return value;
    }
    // The replacer re-reads every property (`this[key]`), which ran each getter twice, so it is
    // used only when one of the two prototypes actually owns a `toJSON`.
    function PRISTINE_STRINGIFY(v) {
        return (hasOwn(OBJECT_PROTO, 'toJSON') || hasOwn(ARRAY_PROTO, 'toJSON'))
            ? NATIVE_STRINGIFY(v, neutralReplacer) : NATIVE_STRINGIFY(v);
    }

    // A deep, detached copy of a JSON-shaped value (IPC args/results): the logs hand out copies
    // so page script cannot rewrite what was captured through a returned reference.
    function cloneJson(v) {
        if (v === null || typeof v !== 'object') return v;
        try { var s = PRISTINE_STRINGIFY(v); return s === undefined ? null : PRISTINE_PARSE(s); }
        catch (e) { return null; }
    }

    // Truncate to at most `n` UTF-16 code units without splitting a surrogate pair: a lone
    // surrogate serializes as an escape that serde_json rejects, failing the whole tool call.
    function truncText(s, n) {
        s = '' + s;
        var start = 0;
        var c0 = s.charCodeAt(0);
        if (c0 >= 0xDC00 && c0 <= 0xDFFF) start = 1; // a leading lone low surrogate
        if (s.length - start <= n) return start ? s.substring(start) : s;
        var end = start + n;
        var last = s.charCodeAt(end - 1);
        if (last >= 0xD800 && last <= 0xDBFF) end--; // would end on a high surrogate
        return s.substring(start, end);
    }

    // ── Eval bookkeeping (closure-private) ──────────────────────────────────
    //
    // The per-eval state lives in this closure, reachable only through the frozen `__VICTAURI__`
    // methods below, which never reveal an id, and serialization uses the `JSON.stringify`
    // captured here at init. That keeps a buggy page from disturbing results. It does NOT stop a
    // hostile page: eval code runs in the page's realm, and results leave through Tauri's `invoke`
    // and so the page's `fetch` / `Headers`, which page script can wrap to read, rewrite or drop a
    // result. Results and logs are evidence against a buggy page, not a hostile one.
    // Identity of THIS page load, reported with the ready signal and the liveness probe. An eval
    // is armed in one page; only a ready signal carrying a DIFFERENT nonce can be a reload that
    // killed it (a late ready from the same page, or one forged by page script, cannot).
    var PAGE_NONCE = (function() {
        try { if (window.crypto && typeof window.crypto.randomUUID === 'function') return window.crypto.randomUUID(); } catch (e) {}
        return Date.now().toString(36) + '-' + Math.random().toString(36).slice(2) + Math.random().toString(36).slice(2);
    })();
    // Evals whose code has begun, and tombstones of settled ones (bounded, oldest evicted), so an
    // outcome is delivered at most once and a "never began" report can never be followed by a run.
    // Both are null-prototype tables behind closure functions, never a `Set`/`Map`: prototype
    // methods are resolved at call time, so page script hooking `Set.prototype.add` would learn
    // every pending eval id and could settle it with a forged result first.
    var EVAL_DONE_CAP = 1000;
    var evalState = (function() {
        var table = OBJ_CREATE(null);
        return {
            has: function(k) { return hasOwn(table, k); },
            add: function(k) { table[k] = true; },
            delete: function(k) { delete table[k]; },
        };
    })();
    var evalDone = (function() {
        var table = OBJ_CREATE(null);
        var ring = [];
        var pos = 0;
        return {
            has: function(k) { return hasOwn(table, k); },
            add: function(k) {
                if (hasOwn(table, k)) return;
                var evicted = ring[pos];
                if (evicted !== undefined) delete table[evicted];
                ring[pos] = k;
                pos = (pos + 1) % EVAL_DONE_CAP;
                table[k] = true;
            },
        };
    })();
    function evalMarkDone(id) {
        evalState.delete(id);
        evalDone.add(id);
    }
    // `body` is always a JSON string. The args object has NO prototype: Tauri serializes it with
    // the page's JSON.stringify, which consults `toJSON` up the prototype chain, so a page that
    // planted a throwing `Object.prototype.toJSON` blocked every outcome (R5-JS2). Only string
    // primitives are inside, and JSON.stringify never looks up `toJSON` on a primitive.
    function evalCallback(id, body) {
        'use strict';
        try {
            var args = OBJ_CREATE(null);
            args.id = id;
            args.result = body;
            return window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', args);
        } catch (e) { return null; }
    }
    // A JSON string literal for `s`, built without serializing any object (see evalCallback).
    function jsonStringLiteral(s) {
        'use strict';
        try { return NATIVE_STRINGIFY(typeof s === 'string' ? s : toStringExact(s)); }
        catch (e) { return '"(unprintable)"'; }
    }
    function errorText(e) {
        'use strict';
        try {
            var m = e && e.message;
            return typeof m === 'string' ? m : toStringExact(e);
        } catch (x) { return 'unknown error'; }
    }
    // The callback body for an eval outcome. Every envelope is assembled as a string around
    // pristine-serialized parts, so nothing but the eval's own result value is serialized as an
    // object — a page that breaks object serialization cannot also break the error report.
    function evalOutcomeBody(payload) {
        'use strict';
        // Own fields only: the error envelope has neither, and a planted
        // `Object.prototype.__victauri_type` turned a thrown eval into a forged success.
        var isObj = payload !== null && typeof payload === 'object';
        var type = isObj && hasOwn(payload, '__victauri_type') ? payload.__victauri_type : undefined;
        if (type === 'value') {
            // The code RAN; a result JSON cannot carry (circular, BigInt, a function, a
            // throwing toJSON) is reported as exactly that, never as a JavaScript error.
            var value = hasOwn(payload, '__victauri_ok') ? payload.__victauri_ok : null;
            var json;
            try { json = PRISTINE_STRINGIFY(value); }
            catch (e) { return '{"__victauri_unserializable":' + jsonStringLiteral(errorText(e)) + '}'; }
            if (json === undefined) {
                return '{"__victauri_unserializable":' + jsonStringLiteral('the result is a ' + typeof value + ', which JSON cannot represent') + '}';
            }
            return '{"__victauri_ok":' + json + ',"__victauri_type":"value"}';
        }
        if (type === 'null' || type === 'undefined') {
            return '{"__victauri_ok":null,"__victauri_type":"' + type + '"}';
        }
        if (isObj && hasOwn(payload, '__victauri_err')) {
            return '{"__victauri_err":' + jsonStringLiteral(payload.__victauri_err) + '}';
        }
        return PRISTINE_STRINGIFY(payload);
    }

    // The bridge's logs are handed out as per-entry COPIES. Returning the live internal arrays
    // (as the getters used to) let page script push forged entries (a fake successful IPC call
    // that `recording replay` would then invoke), splice out its own traffic, or plant values
    // that break every later read. Freezing the bridge object does not protect its arrays.
    // The copies are DEEP: an entry's nested objects (IPC request args, response bodies, route
    // headers) are cloned too, else page script rewrote a logged call's arguments or result
    // through the returned reference.
    // A plain array of the entries at or after `since`, picked by index (see appendCapped).
    function entriesSince(arr, since) {
        var out = [];
        for (var i = 0; i < arr.length; i++) {
            if (!since || arr[i].timestamp >= since) out[out.length] = arr[i];
        }
        return out;
    }
    function copyEntries(arr) {
        var out = new Array(arr.length);
        for (var i = 0; i < arr.length; i++) {
            var e = arr[i];
            if (e && typeof e === 'object') {
                var c = nullProto(e);
                for (var k in c) {
                    if (hasOwn(c, k) && c[k] !== null && typeof c[k] === 'object') c[k] = cloneJson(c[k]);
                }
                out[i] = c;
            } else {
                out[i] = e;
            }
        }
        return out;
    }

    // A network-log entry's IPC command name, or null when it is not an app IPC call
    // (plain network traffic, or Victauri's own `plugin:victauri|*` plumbing).
    var IPC_LOG_VICTAURI_PREFIX = 'plugin%3Avictauri%7C';
    function safeDecode(raw) {
        try { return DECODE_URI_COMPONENT(raw); } catch (e) { return raw; }
    }
    function ipcCallCommand(n) {
        var raw = ipcEntryPath(n);
        if (raw === null || REFLECT_APPLY(STR_INDEXOF, raw, [IPC_LOG_VICTAURI_PREFIX]) === 0) return null;
        return safeDecode(raw);
    }
    // The newest app IPC call in the network log (the live entry — never hand it out).
    function newestIpcCall() {
        for (var i = networkLog.length - 1; i >= 0; i--) {
            if (ipcCallCommand(networkLog[i]) !== null) return networkLog[i];
        }
        return null;
    }
    // Whether a call's `args` are not what was sent: a marker in their place (over the capture
    // cap, encrypted), or a call that carried arguments which were not captured at all (a binary
    // body, capture off), which would otherwise read as a call with no arguments.
    function ipcArgsIncomplete(n) {
        var a = n.request_args;
        if (typeof a === 'string') {
            return REFLECT_APPLY(STR_INDEXOF, a, ['[body omitted:']) === 0
                || REFLECT_APPLY(STR_INDEXOF, a, ['[encrypted:']) === 0;
        }
        return (a === undefined || a === null) && n.arg_size_bytes > 0;
    }

    // The `getIpcLog` view of one IPC network entry: a fresh object whose nested values are
    // deep copies, so the page cannot rewrite a logged call through it. Without `bodies`, the
    // request args and result are left out and the error text is capped.
    var MAX_IPC_ERROR_TEXT = 4096;
    // The outcome of one IPC entry, by Tauri's own rule (ipc-protocol.js): `invoke` resolves only
    // when `Tauri-Response` is 'ok' and rejects otherwise — the HTTP status is 200 for a failed
    // command too, and a mock's status is ignored entirely. Every Tauri 2/3 POST answer that
    // reached a handler carries the header; a 200 without one (an OPTIONS preflight, say) ran no
    // command, so it is 'unknown' — never proof that a handler exists.
    function ipcOutcome(n) {
        if (n.status === 'pending') return 'pending';
        // A postMessage call whose response Victauri cannot observe: outcome unknown.
        if (n.status === 'unobserved') return 'unknown';
        if (n.mocked !== true && n.status !== 200 && n.status !== 'ok') return 'error';
        if (n.ipc_response === 'ok') return 'ok';
        if (n.ipc_response === 'error') return 'error';
        return n.mocked === true ? 'error' : 'unknown';
    }
    function ipcLogEntry(n, bodies) {
        var st = ipcOutcome(n);
        var errText = null;
        if (st === 'error') {
            if (n.mocked !== true && n.status !== 200 && n.status !== 'ok' && n.status !== 'pending') {
                errText = 'HTTP ' + n.status;
            } else if (n.response_body != null) {
                // Command-level error: the body carries the error message.
                errText = typeof n.response_body === 'string'
                    ? n.response_body : PRISTINE_STRINGIFY(n.response_body);
            } else {
                errText = 'command error';
            }
            if (!bodies && typeof errText === 'string') errText = truncText(errText, MAX_IPC_ERROR_TEXT);
        }
        var e = nullProto({
            id: n.id,
            command: ipcCallCommand(n),
            timestamp: n.timestamp,
            status: st,
            duration_ms: n.duration_ms,
            error: errText,
        });
        // Only non-default transports are tagged (fetch is the default; no field = fetch).
        if (n.transport === 'post_message') e.transport = 'post_message';
        // Two different commands were serialized under this call's callback ids: page script
        // staged one of them, and which is the real one is unknowable.
        if (n.ids_contested === true) {
            e.ids_contested = true;
            e.contested_commands = cloneJson(n.contested_commands);
        }
        if (typeof n.mock_unreadable === 'string') e.mock_unreadable = n.mock_unreadable;
        // A `route` rule answered this call in the page (`mocked`: the backend never ran) or
        // held it back (`route_delay_ms` is part of `duration_ms`). Analysis built on the log
        // (ghost detection, command_catalog, coverage, timings) skips mocked calls.
        if (n.mocked === true) e.mocked = true;
        if (typeof n.route_delay_ms === 'number') e.route_delay_ms = n.route_delay_ms;
        if (typeof n.route_rule === 'number') e.route_rule = n.route_rule;
        if (bodies) {
            e.args = cloneJson(n.request_args) || {};
            if (ipcArgsIncomplete(n)) e.args_truncated = true;
            // Nullish, not falsy: a command returning 0 / false / '' is a real result.
            e.result = (n.response_body === undefined || n.response_body === null) ? null : cloneJson(n.response_body);
        }
        return e;
    }

    // ── Event stream (shared by getEventStream and the recording drain) ─────
    //
    // Calls `emit(event, keyTime, entry)` for every loggable entry. IPC and plain network
    // entries are keyed by COMPLETION time: keyed by start, a call still pending at one drain
    // tick fell behind the watermark and stayed "pending" in recordings forever. With
    // `skipPending` a pending call is not emitted at all (it is emitted once it completes).
    // `mocked` marks a request a route rule answered (fulfill) or refused (block) in the page —
    // it never reached the backend, so it must never be replayed as a real call.
    var DRAIN_INSTANCE = (function() {
        try {
            var b = new Uint32Array(4);
            window.crypto.getRandomValues(b);
            return Array.prototype.map.call(b, function(x) { return x.toString(36); }).join('-');
        } catch (e) {
            return Date.now().toString(36) + '-' + Math.random().toString(36).slice(2);
        }
    })();
    var drainSeq = 0;
    var drainSeqs = new WeakMap();
    var IPC_VICTAURI_PREFIX = 'plugin%3Avictauri%7C';
    function forEachStreamEvent(skipPending, emit) {
        // Indexed, not `.forEach`: a page hook on Array.prototype.forEach would get the live log
        // (see appendCapped).
        for (var ci = 0; ci < consoleLogs.length; ci++) {
            var l = consoleLogs[ci];
            emit({ type: 'console', level: l.level, message: l.message, timestamp: l.timestamp }, l.timestamp, l);
        }
        for (var mi = 0; mi < mutationLog.length; mi++) {
            var mu = mutationLog[mi];
            emit({ type: 'dom_mutation', count: mu.count, timestamp: mu.timestamp }, mu.timestamp, mu);
        }
        for (var ni = 0; ni < networkLog.length; ni++) {
            var n = networkLog[ni];
            var isPending = n.status === 'pending';
            if (isPending && skipPending) continue;
            var key = isPending ? n.timestamp : n.timestamp + (n.duration_ms || 0);
            var mocked = n.mocked === true || n.blocked === true;
            var raw = ipcEntryPath(n);
            if (raw === null) {
                // Plain network traffic. IPC requests are emitted once, as `ipc` events —
                // not a second time as `network`.
                var nev = { type: 'network', method: n.method, url: n.url, status: n.status, duration_ms: n.duration_ms, timestamp: n.timestamp, seq_ts: key };
                if (mocked) nev.mocked = true;
                emit(nev, key, n);
                continue;
            }
            if (REFLECT_APPLY(STR_INDEXOF, raw, [IPC_VICTAURI_PREFIX]) === 0) continue;
            // Same classification as getIpcLog (Tauri's own reading of the response).
            var iev = { type: 'ipc', command: safeDecode(raw), status: ipcOutcome(n), duration_ms: n.duration_ms, arg_size_bytes: n.arg_size_bytes || 0, timestamp: n.timestamp, seq_ts: key };
            if (mocked) iev.mocked = true;
            if (n.transport === 'post_message') iev.transport = 'post_message';
            emit(iev, key, n);
        }
        for (var vi = 0; vi < navigationLog.length; vi++) {
            var nv = navigationLog[vi];
            emit({ type: 'navigation', url: nv.url, nav_type: nv.type, timestamp: nv.timestamp }, nv.timestamp, nv);
        }
        for (var ii = 0; ii < interactionLog.length; ii++) {
            var ia = interactionLog[ii];
            emit({ type: 'dom_interaction', action: ia.action, selector: ia.selector, value: ia.value, timestamp: ia.timestamp }, ia.timestamp, ia);
        }
    }

    // ── Public API ───────────────────────────────────────────────────────────

    window.__VICTAURI__ = {
        version: '__VICTAURI_BRIDGE_VERSION__',
        _captureIpcBodies: true,

        // Eval plumbing used by Victauri's injected eval scripts (not a user API).
        _pageNonce: PAGE_NONCE,
        // Called synchronously at the top of the eval wrapper. Returns false when the eval was
        // already settled (reported as never begun), in which case the wrapper must not run it.
        _evalBegin: function(id) {
            'use strict';
            id = '' + id; // not String(id): page script can replace window.String
            if (evalDone.has(id)) return false;
            evalState.add(id);
            return true;
        },
        // Delivered right AFTER the wrapper script (webview evals run in order). If the wrapper
        // never began, it failed to parse — almost always a syntax error. No timer is involved,
        // so a delayed delivery can never report a parse error for code that then runs. `nonce`
        // is the page the eval was armed in: a check that lands in another page stays silent.
        _evalCheck: function(id, nonce) {
            'use strict';
            id = '' + id; // not String(id): page script can replace window.String
            if (nonce !== PAGE_NONCE || evalState.has(id) || evalDone.has(id)) return null;
            evalMarkDone(id);
            return evalCallback(id, '{"__victauri_not_run":' + jsonStringLiteral('the code did not begin executing — this almost always means a syntax/parse error in the submitted code') + '}');
        },
        // The result body of a page-originated eval (`victauri_eval_js`, see page_eval_script):
        // the value's JSON (`null` when JSON has no representation) or `{"__error": message}`,
        // serialized with the stringify captured at init. Never throws: a value that cannot be
        // serialized (circular, BigInt, a throwing own `toJSON`) is reported as an error, not
        // left to time out (R5B-PRISTINE1).
        _pageEvalBody: function(value, isError) {
            'use strict';
            if (isError) return '{"__error":' + jsonStringLiteral(errorText(value)) + '}';
            var json;
            try { json = PRISTINE_STRINGIFY(value); }
            catch (e) {
                return '{"__error":' + jsonStringLiteral('the result could not be serialized: ' + errorText(e)) + '}';
            }
            return json === undefined ? 'null' : json;
        },
        // Deliver an eval's outcome exactly once (a later settle for the same id is ignored).
        _evalSettle: function(id, payload) {
            'use strict';
            id = '' + id; // not String(id): page script can replace window.String
            if (evalDone.has(id)) return null;
            evalMarkDone(id);
            // Marked done first, so an outcome MUST be sent from here on: nothing below may
            // throw past this point without one (R5-JS2).
            var body;
            try { body = evalOutcomeBody(payload); }
            catch (e) { body = undefined; }
            if (typeof body !== 'string') {
                body = '{"__victauri_err":' + jsonStringLiteral('the eval outcome could not be serialized') + '}';
            }
            return evalCallback(id, body);
        },

        // ── DOM ──────────────────────────────────────────────────────────────

        snapshot: function(format) {
            var previousRefs = [];
            REFLECT_APPLY(MAP_FOREACH, refMap, [function(v, k) { previousRefs[previousRefs.length] = k; }]);
            REFLECT_APPLY(MAP_CLEAR, refMap, []);
            var fmt = format || 'compact';
            var tree;
            var currentRefs = OBJ_CREATE(null);
            buildingRefs = currentRefs;
            try {
                if (fmt === 'json') {
                    tree = walkDom(document.body, true);
                } else {
                    tree = walkDomCompact(document.body, 0, true);
                }
            } finally {
                buildingRefs = null;
            }
            var stale = [];
            var staleSet = OBJ_CREATE(null);
            function markStale(rid) {
                if (!staleSet[rid]) { staleSet[rid] = true; stale[stale.length] = rid; }
            }
            for (var pi = 0; pi < previousRefs.length; pi++) {
                var refId = previousRefs[pi];
                if (!currentRefs[refId]) {
                    var weak = REFLECT_APPLY(MAP_GET, weakRefMap, [refId]);
                    if (weak) {
                        var el = REFLECT_APPLY(WEAKREF_DEREF, weak, []);
                        if (!el || !el.isConnected) {
                            markStale(refId);
                            REFLECT_APPLY(MAP_DELETE, weakRefMap, [refId]);
                        }
                    } else {
                        markStale(refId);
                    }
                }
            }
            // Prune dead or detached entries so weakRefMap stays bounded by the
            // live DOM (refs of the snapshot just built are never pruned).
            REFLECT_APPLY(MAP_FOREACH, weakRefMap, [function(w, rid) {
                if (currentRefs[rid]) return;
                var e2 = REFLECT_APPLY(WEAKREF_DEREF, w, []);
                if (!e2 || !e2.isConnected) {
                    REFLECT_APPLY(MAP_DELETE, weakRefMap, [rid]);
                    REFLECT_APPLY(MAP_DELETE, refMap, [rid]);
                    markStale(rid);
                }
            }]);
            return { tree: tree, stale_refs: stale, format: fmt };
        },

        getRef: function(refId) {
            return resolveRef(refId);
        },

        getStaleRefs: function() {
            return getStaleRefs();
        },

        // The "from another page load" error for `refId`, or null when it is this page's ref.
        foreignRefError: function(refId) {
            return foreignRefError(refId);
        },

        // The page's text as a user reads it: rendered text (`innerText`) plus open shadow roots
        // and same-origin frames; '' while the page has no body. What `wait_for text` searches.
        getPageText: function() {
            return pageText(document.body);
        },

        // Query keys (all optional): css, role, test_id, tag, name (accessible name), text
        // (substring, case-insensitive), text_exact, exact, placeholder, alt, title_attr, label,
        // attribute + attribute_value, enabled, include_hidden, max_results (default 10, 0-5000).
        // Filters apply before the cap. Text matches the normalised full text and returns only the
        // DEEPEST matches, so ancestors neither shadow the element nor consume `max_results`.
        findElements: function(query) {
            var q = ownCopy(query);
            var results = [];
            var maxResults = typeof q.max_results === 'number' ? Math.max(0, Math.min(q.max_results, 5000)) : 10;
            var includeHidden = q.include_hidden === true;
            // `exact` is the older spelling of text_exact.
            var legacyExact = !!q.text && q.exact === true;
            // Normalised like the text it is compared with (a blank needle stays as given: it must
            // not become '', which every element contains).
            var textNeedle = q.text && !legacyExact ? (normText('' + q.text) || ('' + q.text)).toLowerCase() : null;
            var textExact = typeof q.text_exact === 'string' ? normText(q.text_exact)
                : (legacyExact ? normText('' + q.text) : null);
            var byText = textNeedle !== null || textExact !== null;

            if (q.css) {
                // Validate against a detached fragment, not document.body: while the page is
                // reloading `body` is null, and the resulting TypeError used to be reported as
                // "invalid CSS selector" for a perfectly valid selector.
                try { document.createDocumentFragment().querySelector(q.css); } catch(e) {
                    return { error: 'invalid CSS selector: ' + q.css + ' — ' + e.message };
                }
            }
            if (q.attribute_value !== undefined && q.attribute_value !== null && !q.attribute) {
                return { error: 'attribute_value needs attribute (the attribute name to compare)' };
            }
            if (!document.body) {
                return { error: 'page not ready: the document has no body yet (it may be loading or reloading) — retry shortly' };
            }
            if (maxResults === 0) return results;

            function matches(el) {
                var tag = elTag(el);
                if (byText) {
                    if (TEXTLESS_TAGS[tag] === true) return false;
                    var txt = normText(nodeFullText(el));
                    if (textExact !== null && txt !== textExact) return false;
                    if (textNeedle !== null && txt.toLowerCase().indexOf(textNeedle) === -1) return false;
                }
                if (q.role) {
                    if ((elAttr(el, 'role') || inferRole(el)) !== q.role) return false;
                }
                if (q.test_id) {
                    if (elAttr(el, 'data-testid') !== q.test_id) return false;
                }
                if (q.css) {
                    if (!REFLECT_APPLY(EL_MATCHES, el, [q.css])) return false;
                }
                if (q.attribute) {
                    if (!elHasAttr(el, q.attribute)) return false;
                    if (q.attribute_value !== undefined && q.attribute_value !== null
                        && elAttr(el, q.attribute) !== '' + q.attribute_value) return false;
                }
                if (q.name) {
                    if ((accessibleName(el) || '').toLowerCase().indexOf(q.name.toLowerCase()) === -1) return false;
                }
                if (q.tag) {
                    if (tag.toLowerCase() !== q.tag.toLowerCase()) return false;
                }
                if (q.placeholder) {
                    if ((elAttr(el, 'placeholder') || '').toLowerCase().indexOf(q.placeholder.toLowerCase()) === -1) return false;
                }
                if (q.alt) {
                    if ((elAttr(el, 'alt') || '').toLowerCase().indexOf(q.alt.toLowerCase()) === -1) return false;
                }
                if (q.title_attr) {
                    if ((elAttr(el, 'title') || '').toLowerCase().indexOf(q.title_attr.toLowerCase()) === -1) return false;
                }
                if (q.enabled === true && !isElEnabled(el)) return false;
                if (q.enabled === false && isElEnabled(el)) return false;
                return true;
            }

            function buildResult(node, cssVisible) {
                // registerRef reuses the node's existing id (if any).
                var ref_id = registerRef(node);
                var rect = elRect(node);
                return {
                    ref_id: ref_id,
                    tag: elTag(node).toLowerCase(),
                    role: elAttr(node, 'role') || inferRole(node),
                    name: accessibleName(node),
                    text: truncText(normText(nodeFullText(node)), 100),
                    bounds: { x: Math.round(rect.x), y: Math.round(rect.y), width: Math.round(rect.width), height: Math.round(rect.height) },
                    visible: cssVisible && (rect.width > 0 && rect.height > 0),
                    enabled: isElEnabled(node),
                    value: isPasswordInput(node) ? '[REDACTED]' : safeValue(node)
                };
            }

            if (q.label) {
                var labels = document.querySelectorAll('label');
                var needle = ('' + q.label).toLowerCase();
                var seen = [];
                for (var li = 0; li < labels.length && results.length < maxResults; li++) {
                    var lbl = labels[li];
                    if ((lbl.textContent || '').toLowerCase().indexOf(needle) === -1) continue;
                    var target = null;
                    var forAttr = lbl.getAttribute('for');
                    if (forAttr) {
                        target = document.getElementById(forAttr);
                    }
                    if (!target) {
                        target = lbl.querySelector('input, textarea, select');
                    }
                    if (!target || !matches(target)) continue;
                    // Two labels can name one control, and a hidden control is no match unless
                    // asked for, as in every other query.
                    var dup = false;
                    for (var si = 0; si < seen.length; si++) { if (seen[si] === target) dup = true; }
                    if (dup) continue;
                    seen[seen.length] = target;
                    var labelledShown = cssVisibleDeep(target);
                    if (labelledShown || includeHidden) results[results.length] = buildResult(target, labelledShown);
                }
                return results;
            }

            // Returns whether this subtree holds a match. Text queries decide a node AFTER its
            // subtree: a node containing a match is not itself one. Results stay in document order
            // (deepest matches are disjoint, so post-order among them is document order).
            function search(node, hiddenAbove) {
                if (results.length >= maxResults) return true;
                if (!node || nodeKind(node) !== 1) return false;
                var style = window.getComputedStyle(node);
                var noneHere = hiddenAbove || style.display === 'none';
                if (noneHere && !includeHidden) return false;
                var hiddenHere = style.visibility === 'hidden' || style.visibility === 'collapse';
                var shown = !noneHere && !hiddenHere;
                var eligible = shown || includeHidden;

                if (!byText && eligible && matches(node)) {
                    results[results.length] = buildResult(node, shown);
                }
                var found = false;
                var kids = elKids(node);
                for (var c = 0; c < kids.length; c++) {
                    if (search(kids[c], noneHere)) found = true;
                }
                var sr = elShadow(node);
                if (sr) {
                    for (var s = 0; s < sr.children.length; s++) {
                        if (search(sr.children[s], noneHere)) found = true;
                    }
                }
                // Same-origin iframe traversal.
                var tn = elTag(node);
                if (tn === 'IFRAME' || tn === 'FRAME') {
                    try {
                        var idoc = node.contentDocument;
                        if (idoc && idoc.body && search(idoc.body, noneHere)) found = true;
                    } catch (e) { /* cross-origin: skip */ }
                }
                if (byText && !found && eligible && results.length < maxResults && matches(node)) {
                    results[results.length] = buildResult(node, shown);
                    found = true;
                }
                return found;
            }

            search(document.body, false);
            return results;
        },

        // ── Interactions ─────────────────────────────────────────────────────

        click: function(refId, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                // A click at a point lands on what is hit there. On a wrapper over an `opacity:0`
                // input the hit is the input, and `wrapper.click()` would report ok and toggle nothing.
                activate(hitDescendant(el) || el);
                return { ok: true };
            });
        },

        doubleClick: function(refId, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true, composed: true }));
                return { ok: true };
            });
        },

        hover: function(refId, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                el.dispatchEvent(new MouseEvent('mouseenter', { bubbles: true, composed: true }));
                el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true, composed: true }));
                return { ok: true };
            });
        },

        fill: function(refId, value, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                var refused = refuseEdit(el);
                if (refused) return refused;
                var text = '' + value;
                var win = el.ownerDocument.defaultView || window;
                if (editKind(el) === 'contenteditable') {
                    el.textContent = text;
                    el.dispatchEvent(new win.Event('input', { bubbles: true, composed: true }));
                    return { ok: true };
                }
                var set = valueSetter(el);
                var before = el.value;
                set(text);
                var rejected = sanitizerRejection(el, text);
                if (rejected) { set(before); return rejected; }
                el.dispatchEvent(new win.Event('input', { bubbles: true, composed: true }));
                el.dispatchEvent(new win.Event('change', { bubbles: true }));
                return { ok: true };
            });
        },

        type: function(refId, text, timeoutMs) {
            text = '' + text;
            // Each character re-sets the whole value and runs the page's handlers, so the cost is
            // quadratic: 100 KB froze the page for 7 s, 1 MB for over 10 minutes.
            if (text.length > MAX_TYPE_CHARS) {
                return Promise.resolve({ ok: false, error: 'text too long for type (' + text.length + ' chars, limit ' + MAX_TYPE_CHARS + '): use fill', hint: 'CHECK_INPUT' });
            }
            return withAutoWait(refId, timeoutMs, function(el) {
                var refused = refuseEdit(el);
                if (refused) return refused;
                var win = el.ownerDocument.defaultView || window;
                var kind = editKind(el);
                el.focus();
                function keys(ch, type) { el.dispatchEvent(new win.KeyboardEvent(type, keyInit(ch, null))); }
                function inputEvent(data) { el.dispatchEvent(new win.InputEvent('input', { bubbles: true, composed: true, data: data, inputType: 'insertText' })); }
                var i, ch;
                if (kind === 'sanitized') {
                    // The browser clears a value that is invalid so far (`-`, `2026-`), so these
                    // types get their whole value once. Only a number extends what is there.
                    var before = el.value;
                    for (i = 0; i < text.length; i += ch.length) {
                        ch = charAtPoint(text, i);
                        keys(ch, 'keydown'); keys(ch, 'keypress'); keys(ch, 'keyup');
                    }
                    var set = valueSetter(el);
                    var full = (inputType(el) === 'number' ? before : '') + text;
                    set(full);
                    var rejected = sanitizerRejection(el, full);
                    if (rejected) { set(before); return rejected; }
                    inputEvent(text);
                } else if (kind === 'contenteditable') {
                    var tn = el.ownerDocument.createTextNode('');
                    el.appendChild(tn);
                    for (i = 0; i < text.length; i += ch.length) {
                        ch = charAtPoint(text, i);
                        keys(ch, 'keydown'); keys(ch, 'keypress');
                        tn.data += ch;
                        inputEvent(ch);
                        keys(ch, 'keyup');
                    }
                    return { ok: true };
                } else {
                    var setText = valueSetter(el);
                    for (i = 0; i < text.length; i += ch.length) {
                        ch = charAtPoint(text, i);
                        keys(ch, 'keydown'); keys(ch, 'keypress');
                        // Reads the live value, as real typing extends what the app left there.
                        setText((el.value || '') + ch);
                        inputEvent(ch);
                        keys(ch, 'keyup');
                    }
                }
                el.dispatchEvent(new win.Event('change', { bubbles: true }));
                return { ok: true };
            });
        },

        pressKey: function(key) {
            var target = deepActiveElement();
            var mods = { ctrlKey: false, shiftKey: false, altKey: false, metaKey: false };
            var finalKey = key;
            if (key.length > 1 && key.charAt(key.length - 1) === '+') {
                // `Control++`: the key is `+`, and splitting on `+` would lose it.
                finalKey = '+';
                key = key.substring(0, key.length - 2);
            }
            var parts = key === '+' ? [] : key.split('+');
            if (finalKey !== '+') finalKey = parts.pop();
            for (var m = 0; m < parts.length; m++) {
                var mod = parts[m];
                if (mod === 'Control' || mod === 'Ctrl') mods.ctrlKey = true;
                else if (mod === 'Shift') mods.shiftKey = true;
                else if (mod === 'Alt') mods.altKey = true;
                else if (mod === 'Meta' || mod === 'Command' || mod === 'Cmd') mods.metaKey = true;
            }
            var modKeys = [];
            if (mods.ctrlKey) modKeys[modKeys.length] = 'Control';
            if (mods.shiftKey) modKeys[modKeys.length] = 'Shift';
            if (mods.altKey) modKeys[modKeys.length] = 'Alt';
            if (mods.metaKey) modKeys[modKeys.length] = 'Meta';
            for (var i = 0; i < modKeys.length; i++) {
                target.dispatchEvent(new KeyboardEvent('keydown', keyInit(modKeys[i], mods)));
            }
            target.dispatchEvent(new KeyboardEvent('keydown', keyInit(finalKey, mods)));
            target.dispatchEvent(new KeyboardEvent('keyup', keyInit(finalKey, mods)));
            for (var j = modKeys.length - 1; j >= 0; j--) {
                target.dispatchEvent(new KeyboardEvent('keyup', keyInit(modKeys[j], mods)));
            }
            return { ok: true };
        },

        selectOption: function(refId, values, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                if (el.tagName !== 'SELECT') {
                    return { ok: false, error: 'element is not a <select>', hint: 'CHECK_INPUT' };
                }
                // No values is how a `<select multiple>` is cleared; a single select always has one.
                if ((!values || !values.length) && !el.multiple) {
                    return { ok: false, error: 'select_option needs at least one value', hint: 'CHECK_INPUT' };
                }
                values = values || [];
                // Checked before anything changes: assigning a value no option has resets the
                // select to its first option, and a disabled option is not one a user can choose.
                var wanted = OBJ_CREATE(null);
                var v;
                for (v = 0; v < values.length; v++) wanted['' + values[v]] = false;
                var i, opt;
                for (i = 0; i < el.options.length; i++) {
                    opt = el.options[i];
                    if (!(opt.value in wanted)) continue;
                    wanted[opt.value] = true;
                    if (opt.disabled || (opt.parentNode && opt.parentNode.tagName === 'OPTGROUP' && opt.parentNode.disabled)) {
                        return { ok: false, error: 'option is disabled: ' + opt.value, hint: 'CHECK_INPUT' };
                    }
                }
                for (v = 0; v < values.length; v++) {
                    if (wanted['' + values[v]] !== true) {
                        return { ok: false, error: 'no option with value: ' + values[v], hint: 'CHECK_INPUT' };
                    }
                }
                for (i = 0; i < el.options.length; i++) {
                    opt = el.options[i];
                    opt.selected = (opt.value in wanted);
                }
                var win = el.ownerDocument.defaultView || window;
                el.dispatchEvent(new win.Event('input', { bubbles: true, composed: true }));
                el.dispatchEvent(new win.Event('change', { bubbles: true }));
                return { ok: true };
            });
        },

        scrollTo: function(refId, x, y, timeoutMs) {
            if (refId) {
                return withAutoWait(refId, timeoutMs, function(el) {
                    el.scrollIntoView({ behavior: 'smooth', block: 'center' });
                    return { ok: true };
                });
            } else {
                window.scrollTo({ left: x || 0, top: y || 0, behavior: 'smooth' });
                return Promise.resolve({ ok: true });
            }
        },

        focusElement: function(refId, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                el.focus();
                return { ok: true, tag: el.tagName.toLowerCase() };
            });
        },

        // ── IPC Log ──────────────────────────────────────────────────────────

        // The IPC calls in the network log, newest last. `limit` (> 0) keeps the newest
        // `limit`; `opts.bodies === false` leaves out `args` / `result` (for callers that only
        // need names, statuses and timings); `opts.ids` keeps only the calls with those ids.
        // Calls are selected and limited FIRST and only the returned ones are deep-copied:
        // copying every retained body (up to 1000 x 64 KB) and then discarding most of it
        // froze the UI thread for ~1 s on every read (R5-JS1).
        getIpcLog: function(limit, opts) {
            opts = ownCopy(opts);
            var bodies = opts.bodies !== false;
            // `backend_only`: leave out calls a `route` rule answered or blocked in the page —
            // the backend never saw them, so they are no evidence about its handlers.
            // `unrouted`: also leave out calls a `route` rule delayed (their duration includes
            // the injected delay). Analysis (ghosts, catalog, coverage, timings) uses these.
            var unrouted = opts.unrouted === true;
            var backendOnly = unrouted || opts.backend_only === true;
            var idSet = null;
            if (opts.ids) {
                idSet = OBJ_CREATE(null);
                for (var j = 0; j < opts.ids.length; j++) idSet['' + opts.ids[j]] = true;
            }
            var picked = [];
            for (var i = 0; i < networkLog.length; i++) {
                var n = networkLog[i];
                if (idSet && !idSet['' + n.id]) continue;
                if (ipcCallCommand(n) === null) continue;
                if (backendOnly && (n.mocked === true || n.blocked === true)) continue;
                if (unrouted && typeof n.route_delay_ms === 'number') continue;
                picked[picked.length] = n; // not .push: a page hook would get the live entry
            }
            var start = (typeof limit === 'number' && limit > 0 && picked.length > limit) ? picked.length - limit : 0;
            var entries = [];
            for (var k = start; k < picked.length; k++) entries[entries.length] = ipcLogEntry(picked[k], bodies);
            return entries;
        },

        // Which Tauri IPC transport this page uses and how completely Victauri captures it:
        // { transport: 'fetch'|'post_message_fallback'|'post_message'|'unknown',
        //   capture: 'full'|'requests_only'|'none'|'unknown', counters, note }.
        getIpcCaptureStatus: function() { return ipcCaptureStatus(); },

        waitForIpcComplete: function(timeoutMs) {
            // Inspect the newest IPC call in place: copying the whole log to read one entry
            // cost as much as a full `getIpcLog()` (R5-JS1).
            var last = newestIpcCall();
            if (last && last.duration_ms !== null && last.duration_ms !== undefined
                && last.response_body !== null && last.response_body !== undefined) {
                return Promise.resolve(true);
            }
            return new Promise(function(resolve) {
                var timer = setTimeout(function() {
                    var idx = ipcWaiters.indexOf(waiterFn);
                    if (idx !== -1) ipcWaiters.splice(idx, 1);
                    resolve(false);
                }, timeoutMs || 500);
                function waiterFn() {
                    clearTimeout(timer);
                    resolve(true);
                }
                ipcWaiters.push(waiterFn);
            });
        },

        // ── Console ──────────────────────────────────────────────────────────

        getConsoleLogs: function(since) {
            return copyEntries(entriesSince(consoleLogs, since));
        },

        // ── Mutations ────────────────────────────────────────────────────────

        getMutationLog: function(since) {
            return copyEntries(entriesSince(mutationLog, since));
        },

        // ── Network ──────────────────────────────────────────────────────────

        // Filtered and limited BEFORE copying (R5-JS1). `opts.bodies === false` leaves out
        // the captured IPC request args / response bodies (e.g. to count entries cheaply).
        getNetworkLog: function(filter, limit, opts) {
            // Indexed copies, not .filter / .slice on the live log (see appendCapped).
            var log = [];
            for (var li = 0; li < networkLog.length; li++) {
                var le = networkLog[li];
                if (!filter || REFLECT_APPLY(STR_INDEXOF, '' + le.url, ['' + filter]) !== -1) log[log.length] = le;
            }
            if (typeof limit === 'number' && limit > 0 && log.length > limit) {
                var tail = [];
                for (var ti = log.length - limit; ti < log.length; ti++) tail[tail.length] = log[ti];
                log = tail;
            }
            var noBodies = ownCopy(opts).bodies === false;
            var out = new Array(log.length);
            for (var i = 0; i < log.length; i++) {
                var c = nullProto(log[i]);
                // Tauri's callback ids stay internal: handing them to page script let it
                // re-plant a completed call's ids (red team F1).
                delete c.ipc_callback;
                if (noBodies) {
                    delete c.request_args;
                    delete c.response_body;
                }
                out[i] = c;
            }
            return copyEntries(out);
        },

        // ── Network routing (rules are added through the agent-only `addRoute`) ──
        getRouteRules: function() { return copyEntries(routeRules); },

        getRouteMatches: function(limit) {
            var from = (limit > 0 && routeMatchLog.length > limit) ? routeMatchLog.length - limit : 0;
            var picked = [];
            for (var i = from; i < routeMatchLog.length; i++) picked[picked.length] = routeMatchLog[i];
            return copyEntries(picked);
        },

        // Agent-only operations (add / clear route rules, clear logs, dialog auto-responses,
        // animation scrub / sweep recorder) are NOT on
        // this page-visible object: page script could otherwise silently remove the agent's
        // block/mock rules, erase captured evidence, or flip dialog auto-answers. They are
        // handed out only for the per-process key Victauri embeds in its own injected scripts.
        //
        // Strict, like every AGENT_OPS function and the `_eval*` plumbing: in sloppy mode a page
        // hook on any built-in they call (`Array.prototype.filter`, `String.prototype.indexOf`,
        // a `then` getter, ...) could walk `hook.caller` up to the injected script whose source
        // holds the key (R4-JS2). A strict function is never exposed as a `.caller`.
        _agent: function(key) {
            'use strict';
            return key === AGENT_KEY ? AGENT_OPS : null;
        },

        // ── Storage ──────────────────────────────────────────────────────────

        getLocalStorage: function(key) {
            if (key !== undefined && key !== null) {
                var v = localStorage.getItem(key);
                try { return JSON.parse(v); } catch(e) { return v; }
            }
            var obj = {};
            for (var i = 0; i < localStorage.length; i++) {
                var k = localStorage.key(i);
                var val = localStorage.getItem(k);
                try { obj[k] = JSON.parse(val); } catch(e) { obj[k] = val; }
            }
            return obj;
        },

        setLocalStorage: function(key, value) {
            localStorage.setItem(key, typeof value === 'string' ? value : JSON.stringify(value));
            return { ok: true };
        },

        deleteLocalStorage: function(key) {
            localStorage.removeItem(key);
            return { ok: true };
        },

        getSessionStorage: function(key) {
            if (key !== undefined && key !== null) {
                var v = sessionStorage.getItem(key);
                try { return JSON.parse(v); } catch(e) { return v; }
            }
            var obj = {};
            for (var i = 0; i < sessionStorage.length; i++) {
                var k = sessionStorage.key(i);
                var val = sessionStorage.getItem(k);
                try { obj[k] = JSON.parse(val); } catch(e) { obj[k] = val; }
            }
            return obj;
        },

        setSessionStorage: function(key, value) {
            sessionStorage.setItem(key, typeof value === 'string' ? value : JSON.stringify(value));
            return { ok: true };
        },

        deleteSessionStorage: function(key) {
            sessionStorage.removeItem(key);
            return { ok: true };
        },

        getCookies: function() {
            if (!document.cookie) return [];
            return document.cookie.split(';').map(function(c) {
                var parts = c.trim().split('=');
                return { name: parts[0], value: parts.slice(1).join('=') };
            });
        },

        // ── Navigation ───────────────────────────────────────────────────────

        getNavigationLog: function() {
            return copyEntries(navigationLog);
        },

        navigate: function(url) {
            window.location.href = url;
            return { ok: true };
        },

        navigateBack: function() {
            history.back();
            return { ok: true };
        },

        // Where a real click on this element would land (the trusted click probe's point).
        hitPoint: function(el, strict) {
            return hitPoint(el, strict === true);
        },

        // ── Dialogs ──────────────────────────────────────────────────────────

        getDialogLog: function() {
            return copyEntries(dialogLog);
        },

        // ── Combined Event Stream ────────────────────────────────────────────

        // `since` is INCLUSIVE by default (events with timestamp >= since), which is
        // what a user-supplied "since" means. A caller that passes its own watermark
        // (the newest timestamp it already ingested) should pass `exclusive = true`
        // so the newest event is not re-emitted on every poll.
        getEventStream: function(since, exclusive) {
            var events = [];
            var ts = since || 0;
            var excl = exclusive === true;
            function inRange(t) { return excl ? t > ts : t >= ts; }
            forEachStreamEvent(excl, function(ev, key) {
                if (inRange(key)) events.push(ev);
            });
            events.sort(function(a, b) { return a.timestamp - b.timestamp; });
            return events;
        },

        // The recording drain's read: every event not yet handed to the drain, exactly once.
        // Keyed by a per-page monotonic SEQUENCE, not a wall-clock watermark — a watermark on
        // `Date.now()` re-read an event stamped ahead of the Rust clock (a page overriding
        // `Date.now`, fake timers, an NTP step back) on every drain, and lost events logged in
        // the same millisecond as the watermark after the read. An entry is sequenced the first
        // time a drain sees it complete (so IPC and network calls are keyed by completion and
        // never emitted while pending), and `instance` identifies this page load: after a
        // reload the counter restarts, so a sequence from another instance means "from the
        // start". `floorMs` (only on the first read of a recording) skips entries that
        // completed before the recording began. Returns `{ instance, seq, events }`; `seq` is
        // the next `afterSeq`.
        drainEvents: function(afterSeq, instance, floorMs) {
            var after = (instance === DRAIN_INSTANCE && typeof afterSeq === 'number') ? afterSeq : 0;
            var floor = (after === 0 && typeof floorMs === 'number') ? floorMs : 0;
            var events = [];
            forEachStreamEvent(true, function(ev, key, entry) {
                var seq = REFLECT_APPLY(WEAKMAP_GET, drainSeqs, [entry]);
                if (seq === undefined) {
                    seq = ++drainSeq;
                    REFLECT_APPLY(WEAKMAP_SET, drainSeqs, [entry, seq]);
                }
                if (seq <= after) return;
                if (floor > 0 && !(key > floor)) return;
                events.push(ev);
            });
            events.sort(function(a, b) { return a.timestamp - b.timestamp; });
            return { instance: DRAIN_INSTANCE, seq: drainSeq, events: events };
        },

        // ── Wait ─────────────────────────────────────────────────────────────

        waitFor: function(opts) {
            opts = ownCopy(opts);
            return new Promise(function(resolve) {
                // 0 is a real timeout (a single immediate check), not "use the default".
                var timeout = typeof opts.timeout_ms === 'number' && opts.timeout_ms >= 0 ? opts.timeout_ms : 10000;
                var poll = opts.poll_ms || 200;
                var start = Date.now();

                // A selector that can never match is a mistake in the call, not a wait to sit out.
                if ((opts.condition === 'selector' || opts.condition === 'selector_gone') && opts.value) {
                    try { document.createDocumentFragment().querySelector(opts.value); } catch (e) {
                        resolve({ ok: false, error: 'invalid CSS selector: ' + opts.value + ' — ' + e.message, hint: 'CHECK_INPUT', elapsed_ms: 0 });
                        return;
                    }
                }

                function checkInner() {
                    var met = false;
                    // A page with no body yet (mid navigation) has not lost the text or the
                    // element: nothing can be said until it has one.
                    if (opts.condition === 'text' && opts.value) {
                        met = !!document.body && pageText(document.body).indexOf(opts.value) !== -1;
                    } else if (opts.condition === 'text_gone' && opts.value) {
                        met = !!document.body && pageText(document.body).indexOf(opts.value) === -1;
                    } else if (opts.condition === 'selector' && opts.value) {
                        met = !!document.querySelector(opts.value);
                    } else if (opts.condition === 'selector_gone' && opts.value) {
                        met = !!document.body && !document.querySelector(opts.value);
                    } else if (opts.condition === 'url' && opts.value) {
                        met = window.location.href.indexOf(opts.value) !== -1;
                    } else if (opts.condition === 'ipc_idle') {
                        // Blind to the traffic, "no call is pending" would be true of anything.
                        var capture = ipcCaptureStatus();
                        if (capture.capture === 'none') {
                            resolve({ ok: false, error: 'ipc_idle cannot be judged: ' + capture.note, ipc_capture: 'none', elapsed_ms: Date.now() - start });
                            return;
                        }
                        met = true;
                        for (var wi = 0; wi < networkLog.length; wi++) {
                            if (isIpcEntry(networkLog[wi]) && networkLog[wi].status === 'pending') { met = false; break; }
                        }
                    } else if (opts.condition === 'network_idle') {
                        met = true;
                        for (var wj = 0; wj < networkLog.length; wj++) {
                            if (networkLog[wj].status === 'pending') { met = false; break; }
                        }
                    }

                    // The condition is checked BEFORE the deadline test, so the last check lands
                    // at the deadline itself, and never sleeps past it.
                    var elapsed = Date.now() - start;
                    if (met) {
                        resolve({ ok: true, elapsed_ms: elapsed });
                    } else if (elapsed >= timeout) {
                        resolve({ ok: false, error: 'timeout after ' + timeout + 'ms', elapsed_ms: elapsed });
                    } else {
                        setTimeout(check, Math.min(poll, timeout - elapsed));
                    }
                }
                // Later checks run in a bare timer: a throw there (an invalid selector) would leave
                // the promise unsettled until the eval timeout.
                function check() {
                    try { checkInner(); }
                    catch (e) { resolve({ ok: false, error: 'wait_for check failed: ' + (e && e.message), elapsed_ms: Date.now() - start }); }
                }
                check();
            });
        },
        // ── CSS / Style Introspection ────────────────────────────────────────

        getStyles: function(refId, properties) {
            var el = resolveRef(refId);
            if (!el) return { error: refNotFound(refId) };
            var computed = window.getComputedStyle(el);
            var result = {};
            if (properties && properties.length > 0) {
                for (var i = 0; i < properties.length; i++) {
                    result[properties[i]] = computed.getPropertyValue(properties[i]);
                }
            } else {
                var important = ['display','position','width','height','margin','padding',
                    'color','background-color','font-size','font-family','font-weight',
                    'border','border-radius','opacity','visibility','overflow','z-index',
                    'flex-direction','justify-content','align-items','gap','grid-template-columns',
                    'box-shadow','transform','transition','cursor','pointer-events','text-align',
                    'line-height','letter-spacing','white-space','text-overflow','max-width',
                    'max-height','min-width','min-height','top','right','bottom','left'];
                // Interactivity-critical props are always shown (when non-empty),
                // even at 'none'/'hidden'/'auto' — `display:none`,
                // `visibility:hidden`, and `pointer-events:none` are exactly the
                // "why can't I interact with this?" answers, and the compactness
                // filter below would otherwise drop them as if they were defaults.
                var alwaysShow = ['display', 'visibility', 'pointer-events'];
                for (var i = 0; i < important.length; i++) {
                    var v = computed.getPropertyValue(important[i]);
                    var critical = alwaysShow.indexOf(important[i]) !== -1;
                    if (v && v !== '' && (critical
                        || (v !== 'none' && v !== 'normal' && v !== 'auto'
                            && v !== '0px' && v !== 'rgba(0, 0, 0, 0)'))) {
                        result[important[i]] = v;
                    }
                }
            }
            return { ref_id: refId, tag: el.tagName.toLowerCase(), styles: result };
        },

        getBoundingBoxes: function(refIds) {
            var results = [];
            for (var i = 0; i < refIds.length; i++) {
                var el = resolveRef(refIds[i]);
                if (!el) { results.push({ ref_id: refIds[i], error: foreignRefError(refIds[i]) || 'ref not found' }); continue; }
                var rect = el.getBoundingClientRect();
                var computed = window.getComputedStyle(el);
                results.push({
                    ref_id: refIds[i],
                    tag: el.tagName.toLowerCase(),
                    x: Math.round(rect.x),
                    y: Math.round(rect.y),
                    width: Math.round(rect.width),
                    height: Math.round(rect.height),
                    margin: {
                        top: parseInt(computed.marginTop) || 0,
                        right: parseInt(computed.marginRight) || 0,
                        bottom: parseInt(computed.marginBottom) || 0,
                        left: parseInt(computed.marginLeft) || 0,
                    },
                    padding: {
                        top: parseInt(computed.paddingTop) || 0,
                        right: parseInt(computed.paddingRight) || 0,
                        bottom: parseInt(computed.paddingBottom) || 0,
                        left: parseInt(computed.paddingLeft) || 0,
                    },
                    border: {
                        top: parseInt(computed.borderTopWidth) || 0,
                        right: parseInt(computed.borderRightWidth) || 0,
                        bottom: parseInt(computed.borderBottomWidth) || 0,
                        left: parseInt(computed.borderLeftWidth) || 0,
                    },
                });
            }
            return results;
        },

        // ── Visual Debug Overlays ────────────────────────────────────────────

        highlightElement: function(refId, color, label) {
            var el = resolveRef(refId);
            if (!el) return { error: refNotFound(refId) };
            var c = color || 'rgba(255, 0, 0, 0.3)';
            var overlay = document.createElement('div');
            overlay.className = '__victauri_highlight__';
            overlay.setAttribute('data-victauri-ref', refId);
            var rect = el.getBoundingClientRect();
            overlay.style.cssText = 'position:fixed;pointer-events:none;z-index:2147483647;' +
                'border:2px solid ' + c + ';background:' + c + ';' +
                'left:' + rect.left + 'px;top:' + rect.top + 'px;' +
                'width:' + rect.width + 'px;height:' + rect.height + 'px;' +
                'transition:all 0.2s ease;';
            if (label) {
                var tag = document.createElement('span');
                tag.textContent = label;
                tag.style.cssText = 'position:absolute;top:-20px;left:0;background:#222;color:#fff;' +
                    'font-size:11px;padding:2px 6px;border-radius:3px;white-space:nowrap;font-family:monospace;';
                overlay.appendChild(tag);
            }
            document.body.appendChild(overlay);
            return { ok: true, ref_id: refId };
        },

        clearHighlights: function() {
            var overlays = document.querySelectorAll('.__victauri_highlight__');
            for (var i = 0; i < overlays.length; i++) overlays[i].remove();
            return { ok: true, removed: overlays.length };
        },

        // ── CSS Injection ────────────────────────────────────────────────────

        injectCss: function(css) {
            var existing = document.getElementById('__victauri_injected_css__');
            if (existing) existing.remove();
            var style = document.createElement('style');
            style.id = '__victauri_injected_css__';
            style.textContent = css;
            document.head.appendChild(style);
            return { ok: true, length: css.length };
        },

        removeInjectedCss: function() {
            var existing = document.getElementById('__victauri_injected_css__');
            if (!existing) return { ok: true, removed: false };
            existing.remove();
            return { ok: true, removed: true };
        },

        // ── Accessibility Audit ──────────────────────────────────────────────

        auditAccessibility: function() {
            var violations = [];
            var warnings = [];

            // Images without alt text
            var imgs = document.querySelectorAll('img');
            for (var i = 0; i < imgs.length; i++) {
                if (!imgs[i].hasAttribute('alt')) {
                    violations.push({ rule: 'img-alt', severity: 'critical', element: describeEl(imgs[i]),
                        message: 'Image missing alt attribute' });
                } else if (imgs[i].alt.trim() === '') {
                    warnings.push({ rule: 'img-alt-empty', severity: 'minor', element: describeEl(imgs[i]),
                        message: 'Image has empty alt (ok if decorative)' });
                }
            }

            // Form inputs without labels
            var inputs = document.querySelectorAll('input, select, textarea');
            for (var i = 0; i < inputs.length; i++) {
                var inp = inputs[i];
                if (inp.type === 'hidden') continue;
                // A button-like input is named by its value (an image button by its alt).
                if ((inp.type === 'submit' || inp.type === 'button' || inp.type === 'reset') && inp.value) continue;
                if (inp.type === 'image' && inp.getAttribute('alt')) continue;
                // `labels` covers a wrapping <label> as well as `for`.
                var hasLabel = !!inp.labels && inp.labels.length > 0;
                if (!hasLabel && inp.id) {
                    try { hasLabel = !!document.querySelector('label[for=\"' + CSS.escape(inp.id) + '\"]'); }
                    catch(e) { /* malformed id — skip */ }
                }
                var hasAria = inp.getAttribute('aria-label') || inp.getAttribute('aria-labelledby');
                var hasTitle = inp.title;
                var hasPlaceholder = inp.placeholder;
                if (!hasLabel && !hasAria && !hasTitle && !hasPlaceholder) {
                    violations.push({ rule: 'input-label', severity: 'serious', element: describeEl(inp),
                        message: 'Form input has no accessible label' });
                }
            }

            // Buttons without accessible text
            var buttons = document.querySelectorAll('button, [role="button"]');
            for (var i = 0; i < buttons.length; i++) {
                var btn = buttons[i];
                var text = (btn.textContent || '').trim();
                var ariaLabel = btn.getAttribute('aria-label');
                var ariaLabelledBy = btn.getAttribute('aria-labelledby');
                if (!text && !ariaLabel && !ariaLabelledBy && !btn.title) {
                    var hasImg = btn.querySelector('img[alt], svg[aria-label]');
                    if (!hasImg) {
                        violations.push({ rule: 'button-name', severity: 'serious', element: describeEl(btn),
                            message: 'Button has no accessible name' });
                    }
                }
            }

            // Links without text
            var links = document.querySelectorAll('a[href]');
            for (var i = 0; i < links.length; i++) {
                var link = links[i];
                var text = (link.textContent || '').trim();
                var ariaLabel = link.getAttribute('aria-label');
                if (!text && !ariaLabel && !link.title && !link.querySelector('img[alt], svg[aria-label]')) {
                    violations.push({ rule: 'link-name', severity: 'serious', element: describeEl(link),
                        message: 'Link has no accessible text' });
                }
            }

            // Missing document language
            if (!document.documentElement.lang) {
                violations.push({ rule: 'html-lang', severity: 'serious', element: '<html>',
                    message: 'Document missing lang attribute' });
            }

            // Heading hierarchy
            var headings = document.querySelectorAll('h1, h2, h3, h4, h5, h6');
            var prevLevel = 0;
            for (var i = 0; i < headings.length; i++) {
                var level = parseInt(headings[i].tagName.charAt(1));
                if (level > prevLevel + 1 && prevLevel > 0) {
                    warnings.push({ rule: 'heading-order', severity: 'moderate', element: describeEl(headings[i]),
                        message: 'Heading level skipped from h' + prevLevel + ' to h' + level });
                }
                prevLevel = level;
            }

            // Missing page title
            if (!document.title || document.title.trim() === '') {
                violations.push({ rule: 'document-title', severity: 'serious', element: '<head>',
                    message: 'Document has no title' });
            }

            // Color contrast (simplified — checks text elements against backgrounds)
            var textEls = document.querySelectorAll('p, span, a, button, h1, h2, h3, h4, h5, h6, li, td, th, label, div');
            var contrastIssues = 0;
            for (var i = 0; i < textEls.length && contrastIssues < 10; i++) {
                var el = textEls[i];
                if (!el.textContent || el.textContent.trim() === '') continue;
                if (el.children.length > 0 && el.children[0].textContent === el.textContent) continue;
                var cs = window.getComputedStyle(el);
                var fg = parseColor(cs.color);
                var bg = parseColor(cs.backgroundColor);
                if (fg && bg && bg.a > 0) {
                    var ratio = contrastRatio(fg, bg);
                    var fontSize = parseFloat(cs.fontSize);
                    var isBold = parseInt(cs.fontWeight) >= 700;
                    var isLarge = fontSize >= 24 || (fontSize >= 18.66 && isBold);
                    var threshold = isLarge ? 3 : 4.5;
                    if (ratio < threshold) {
                        contrastIssues++;
                        warnings.push({ rule: 'color-contrast', severity: 'serious',
                            element: describeEl(el),
                            message: 'Contrast ratio ' + ratio.toFixed(2) + ':1 (needs ' + threshold + ':1)',
                            details: { fg: cs.color, bg: cs.backgroundColor, ratio: ratio.toFixed(2) } });
                    }
                }
            }

            // ARIA role validity
            var ariaEls = document.querySelectorAll('[role]');
            var validRoles = new Set(['alert','alertdialog','application','article','banner','button',
                'cell','checkbox','columnheader','combobox','complementary','contentinfo','definition',
                'dialog','directory','document','feed','figure','form','grid','gridcell','group',
                'heading','img','link','list','listbox','listitem','log','main','marquee','math',
                'menu','menubar','menuitem','menuitemcheckbox','menuitemradio','meter','navigation',
                'none','note','option','presentation','progressbar','radio','radiogroup','region',
                'row','rowgroup','rowheader','scrollbar','search','searchbox','separator','slider',
                'spinbutton','status','switch','tab','table','tablist','tabpanel','term','textbox',
                'timer','toolbar','tooltip','tree','treegrid','treeitem']);
            for (var i = 0; i < ariaEls.length; i++) {
                var role = ariaEls[i].getAttribute('role');
                if (role && !validRoles.has(role)) {
                    warnings.push({ rule: 'aria-role', severity: 'moderate', element: describeEl(ariaEls[i]),
                        message: 'Invalid ARIA role: ' + role });
                }
            }

            // Tab index > 0
            var tabbable = document.querySelectorAll('[tabindex]');
            for (var i = 0; i < tabbable.length; i++) {
                var ti = parseInt(tabbable[i].getAttribute('tabindex'));
                if (ti > 0) {
                    warnings.push({ rule: 'tabindex-positive', severity: 'moderate', element: describeEl(tabbable[i]),
                        message: 'Positive tabindex disrupts natural tab order (tabindex=' + ti + ')' });
                }
            }

            return {
                violations: violations,
                warnings: warnings,
                summary: {
                    critical: violations.filter(function(v) { return v.severity === 'critical'; }).length,
                    serious: violations.filter(function(v) { return v.severity === 'serious'; }).length + warnings.filter(function(w) { return w.severity === 'serious'; }).length,
                    moderate: warnings.filter(function(w) { return w.severity === 'moderate'; }).length,
                    minor: warnings.filter(function(w) { return w.severity === 'minor'; }).length,
                    total: violations.length + warnings.length,
                }
            };
        },

        // ── Performance Metrics ──────────────────────────────────────────────

        getPerformanceMetrics: function() {
            var result = {};

            // Navigation timing
            var nav = performance.getEntriesByType('navigation')[0];
            if (nav) {
                result.navigation = {
                    dns_ms: Math.round(nav.domainLookupEnd - nav.domainLookupStart),
                    connect_ms: Math.round(nav.connectEnd - nav.connectStart),
                    ttfb_ms: Math.round(nav.responseStart - nav.requestStart),
                    response_ms: Math.round(nav.responseEnd - nav.responseStart),
                    dom_interactive_ms: Math.round(nav.domInteractive - nav.startTime),
                    dom_complete_ms: Math.round(nav.domComplete - nav.startTime),
                    load_event_ms: Math.round(nav.loadEventEnd - nav.startTime),
                    transfer_size: nav.transferSize || 0,
                    encoded_body_size: nav.encodedBodySize || 0,
                    decoded_body_size: nav.decodedBodySize || 0,
                };
            }

            // Resource summary
            var resources = performance.getEntriesByType('resource');
            var byType = {};
            var totalTransfer = 0;
            for (var i = 0; i < resources.length; i++) {
                var r = resources[i];
                var type = r.initiatorType || 'other';
                if (!byType[type]) byType[type] = { count: 0, total_ms: 0, total_bytes: 0 };
                byType[type].count++;
                byType[type].total_ms += r.duration;
                byType[type].total_bytes += r.transferSize || 0;
                totalTransfer += r.transferSize || 0;
            }
            result.resources = {
                total_count: resources.length,
                total_transfer_bytes: totalTransfer,
                by_type: byType,
                slowest: resources.sort(function(a, b) { return b.duration - a.duration; }).slice(0, 5).map(function(r) {
                    return { name: r.name.split('/').pop().split('?')[0], duration_ms: Math.round(r.duration), size: r.transferSize || 0, type: r.initiatorType };
                }),
            };

            // Engine capability probe. Several perf APIs below are Chromium/WebView2-
            // ONLY and are simply undefined on WebKit (WKWebView/macOS, WebKitGTK/Linux)
            // — Victauri's moat platforms. Without this, those fields silently vanish
            // there and an agent reads "no heap / no long tasks / no paint" as real data
            // (and a heap-budget assertion passes regardless of memory). Feature-detect
            // explicitly so the unavailability is reported, never silent.
            var supportedEntryTypes = (typeof PerformanceObserver !== 'undefined' && PerformanceObserver.supportedEntryTypes) || [];
            result.engine = {
                js_heap_supported: typeof performance.memory !== 'undefined',
                long_task_supported: supportedEntryTypes.indexOf('longtask') !== -1,
                paint_timing_supported: supportedEntryTypes.indexOf('paint') !== -1,
                user_agent: navigator.userAgent,
            };

            // Paint timing (Chromium-first; Safari ~14.1; WebKitGTK varies)
            var paints = performance.getEntriesByType('paint');
            if (paints.length === 0 && !result.engine.paint_timing_supported) {
                result.paint = { unavailable: true, reason: 'Paint Timing API not supported on this webview engine' };
            } else {
                result.paint = {};
                for (var i = 0; i < paints.length; i++) {
                    result.paint[paints[i].name] = Math.round(paints[i].startTime);
                }
            }

            // JS heap — performance.memory is Chromium/WebView2-only.
            if (performance.memory) {
                result.js_heap = {
                    used_mb: Math.round(performance.memory.usedJSHeapSize / 1048576 * 100) / 100,
                    total_mb: Math.round(performance.memory.totalJSHeapSize / 1048576 * 100) / 100,
                    limit_mb: Math.round(performance.memory.jsHeapSizeLimit / 1048576 * 100) / 100,
                };
            } else {
                result.js_heap = { unavailable: true, reason: 'performance.memory is Chromium/WebView2-only; undefined on WebKit (WKWebView/WebKitGTK)' };
            }

            // Long tasks — Long Tasks API ('longtask' entry type) is Chromium-only.
            if (longTasks.length > 0) {
                result.long_tasks = {
                    count: longTasks.length,
                    total_ms: Math.round(longTasks.reduce(function(s, t) { return s + t.duration; }, 0)),
                    worst_ms: Math.round(Math.max.apply(null, longTasks.map(function(t) { return t.duration; }))),
                };
            } else if (!result.engine.long_task_supported) {
                result.long_tasks = { unavailable: true, reason: 'Long Tasks API is Chromium-only' };
            } else {
                result.long_tasks = { count: 0, total_ms: 0, worst_ms: 0 };
            }

            // DOM stats
            result.dom = {
                elements: document.querySelectorAll('*').length,
                max_depth: (function() { var d = 0; var walk = function(el, depth) { if (depth > d) d = depth; for (var i = 0; i < el.children.length && i < 5; i++) walk(el.children[i], depth + 1); }; if (document.body) walk(document.body, 0); return d; })(),
                event_listeners: listenerCount,
            };

            return result;
        },

        getDiagnostics: function() {
            var diag = { warnings: [], info: {} };

            // Service worker detection
            if (navigator.serviceWorker && navigator.serviceWorker.controller) {
                diag.warnings.push({
                    id: 'service-worker-active',
                    severity: 'high',
                    message: 'Active service worker detected — may intercept fetch calls to ipc.localhost, causing IPC log gaps',
                    details: { scope: navigator.serviceWorker.controller.scriptURL }
                });
            }

            // Closed shadow DOM detection
            var allEls = document.querySelectorAll('*');
            var closedShadowCount = 0;
            for (var i = 0; i < allEls.length; i++) {
                if (allEls[i].attachShadow && !allEls[i].shadowRoot) {
                    var tagName = allEls[i].tagName.toLowerCase();
                    if (tagName.includes('-')) closedShadowCount++;
                }
            }
            if (closedShadowCount > 0) {
                diag.warnings.push({
                    id: 'closed-shadow-dom',
                    severity: 'medium',
                    message: closedShadowCount + ' custom element(s) may use closed shadow DOM — their contents are invisible to dom_snapshot',
                    details: { count: closedShadowCount }
                });
            }

            // iframe detection
            var iframes = document.querySelectorAll('iframe');
            if (iframes.length > 0) {
                diag.warnings.push({
                    id: 'iframes-present',
                    severity: 'medium',
                    message: iframes.length + ' iframe(s) found — Victauri bridge is not injected inside iframes (Tauri limitation)',
                    details: { count: iframes.length, srcs: Array.from(iframes).slice(0, 5).map(function(f) { return f.src || '(empty)'; }) }
                });
            }

            // DOM size warning
            var elementCount = allEls.length;
            if (elementCount > 5000) {
                diag.warnings.push({
                    id: 'large-dom',
                    severity: 'low',
                    message: 'DOM has ' + elementCount + ' elements — dom_snapshot may be slow (>100ms)',
                    details: { count: elementCount }
                });
            }

            // CSP detection (best-effort)
            var cspMeta = document.querySelector('meta[http-equiv="Content-Security-Policy"]');
            if (cspMeta) {
                var cspContent = cspMeta.getAttribute('content') || '';
                diag.info.csp_meta = cspContent;
                if (cspContent.indexOf('unsafe-eval') === -1 && cspContent.indexOf('script-src') !== -1) {
                    diag.info.csp_note = 'CSP restricts eval — Victauri uses native webview.eval() which bypasses CSP on most platforms';
                }
            }

            // IPC capture completeness: which Tauri IPC transport this page uses and how much
            // of it Victauri sees. A degraded state is a warning — every IPC-derived tool
            // (logs ipc, ghost detection, command_catalog, coverage, timings) depends on it.
            var ipcStatus = ipcCaptureStatus();
            diag.info.ipc_capture = ipcStatus;
            if (ipcStatus.transport === 'post_message_fallback' || ipcStatus.capture === 'requests_only' || ipcStatus.capture === 'none') {
                diag.warnings.push({
                    id: 'ipc-postmessage-fallback',
                    severity: ipcStatus.capture === 'full' ? 'low' : 'high',
                    message: ipcStatus.note,
                    details: { transport: ipcStatus.transport, capture: ipcStatus.capture,
                               fallback_detected_via: ipcStatus.fallback_detected_via }
                });
            }

            // Environment info
            diag.info.bridge_version = window.__VICTAURI__.version;
            diag.info.user_agent = navigator.userAgent;
            diag.info.url = window.location.href;
            diag.info.dom_elements = elementCount;
            diag.info.open_shadow_roots = (function() { var c = 0; for (var i = 0; i < allEls.length; i++) { if (allEls[i].shadowRoot) c++; } return c; })();
            diag.info.event_listeners = listenerCount;
            diag.info.protocol = window.location.protocol;

            return diag;
        },

        // ── Animation Introspection (Web Animations API) ─────────────────────
        // Reads the running CSS animations/transitions so an agent can see what
        // the webview's animation engine is actually doing: declared timing,
        // easing, keyframes, current progress, and the animating element. Pure
        // standard DOM — works identically on WebView2/WKWebView/WebKitGTK.
        listAnimations: function(selector) {
            function rect(el) {
                if (!el || !el.getBoundingClientRect) return null;
                var b = el.getBoundingClientRect();
                return { x: Math.round(b.x), y: Math.round(b.y),
                         w: Math.round(b.width), h: Math.round(b.height) };
            }
            function describe(el) {
                if (!el) return null;
                var cls = (el.className && el.className.toString)
                    ? truncText(el.className.toString(), 60) : null;
                return { tag: el.tagName ? el.tagName.toLowerCase() : null,
                         id: el.id || null, cls: cls, rect: rect(el) };
            }
            var anims;
            try {
                if (selector) {
                    var scope = document.querySelectorAll(selector);
                    anims = [];
                    for (var i = 0; i < scope.length; i++) {
                        if (scope[i].getAnimations) {
                            anims = anims.concat(scope[i].getAnimations());
                        }
                    }
                } else {
                    anims = document.getAnimations ? document.getAnimations() : [];
                }
            } catch (e) {
                return { error: 'getAnimations failed: ' + (e && e.message) };
            }
            return anims.map(function(a) {
                var e = a.effect;
                var t = (e && e.getTiming) ? e.getTiming() : {};
                var ct = (e && e.getComputedTiming) ? e.getComputedTiming() : {};
                var kf = [];
                try { kf = (e && e.getKeyframes) ? e.getKeyframes() : []; } catch (_) {}
                return {
                    type: a.constructor ? a.constructor.name : 'Animation',
                    id: a.id || null,
                    animation_name: a.animationName || null,
                    transition_property: a.transitionProperty || null,
                    play_state: a.playState,
                    current_time: a.currentTime,
                    playback_rate: a.playbackRate,
                    timing: { duration: t.duration, delay: t.delay, end_delay: t.endDelay,
                              easing: t.easing, iterations: t.iterations,
                              direction: t.direction, fill: t.fill },
                    computed: { active_duration: ct.activeDuration, end_time: ct.endTime,
                                progress: ct.progress, current_iteration: ct.currentIteration },
                    target: describe(e && e.target),
                    keyframes: kf
                };
            });
        },
    };

    try {
        Object.freeze(window.__VICTAURI__);
        Object.defineProperty(window, '__VICTAURI__', {
            value: window.__VICTAURI__,
            configurable: false,
            writable: false,
        });
    } catch(e) {}

    // Animation scrub / sweep-recorder state. Closure-held, not `window.__VICTAURI_SCRUB__` /
    // `window.__VICTAURI_SWEEP__` globals, which page script could overwrite to fake the
    // measured animation curve and jank statistics.
    var scrubState = null;
    var sweepState = null;

    // Seeking a paused animation through its delay, its iterations or its end fires the page's own
    // animation events (a toast removes itself, a route transition completes) for what is only a
    // scrub. This capture listener is the bridge's, so it runs before any the page adds, and stops
    // those events for the scrubbed target until two frames after the restore: events are dispatched
    // in the frame after the seek. Synthetic events and other elements are the page's business.
    var SCRUB_EVENTS = ['animationstart', 'animationend', 'animationiteration', 'animationcancel',
        'transitionrun', 'transitionstart', 'transitionend', 'transitioncancel'];
    var scrubQuiet = [];
    function quietScrubEvent(e) {
        if (!scrubQuiet.length || e.isTrusted !== true) return;
        for (var i = 0; i < scrubQuiet.length; i++) {
            if (scrubQuiet[i].el === e.target) { e.stopImmediatePropagation(); return; }
        }
    }
    function listenForScrubEvents(target) {
        for (var i = 0; i < SCRUB_EVENTS.length; i++) target.addEventListener(SCRUB_EVENTS[i], quietScrubEvent, true);
    }
    listenForScrubEvents(window);
    // These events are not composed: from inside a shadow tree they never reach the window.
    function holdScrubEvents(el) {
        var hold = { el: el, root: null };
        var root = typeof el.getRootNode === 'function' ? el.getRootNode() : null;
        if (root && root !== el.ownerDocument && root.host) {
            hold.root = root;
            listenForScrubEvents(root);
        }
        scrubQuiet[scrubQuiet.length] = hold;
        return hold;
    }
    function releaseScrubEvents(hold) {
        var released = false;
        function release() {
            if (released) return;
            released = true;
            for (var i = 0; i < scrubQuiet.length; i++) {
                if (scrubQuiet[i] === hold) { removeAt(scrubQuiet, i); break; }
            }
            if (hold.root) {
                for (var k = 0; k < SCRUB_EVENTS.length; k++) hold.root.removeEventListener(SCRUB_EVENTS[k], quietScrubEvent, true);
            }
        }
        requestAnimationFrame(function() { requestAnimationFrame(release); });
        // rAF does not fire in a hidden or minimized window.
        SET_TIMEOUT(release, 1000);
    }

    // See `_agent`: reachable only with the per-process agent key. Built in a STRICT function so
    // every op — and every callback an op creates — is strict: a page hook on a built-in an op
    // calls then cannot reach the op through `.caller` (and call it without the key), nor the
    // injected script beyond it whose source carries the key (R4-JS2).
    var AGENT_OPS = (function() {
    'use strict';
    var AGENT_OPS = OBJ_CREATE(null);
    AGENT_OPS.clearIpcLog = function() {
        for (var i = networkLog.length - 1; i >= 0; i--) {
            if (isIpcEntry(networkLog[i])) removeAt(networkLog, i);
        }
        return { ok: true };
    };
    AGENT_OPS.clearNetworkLog = function() { networkLog.length = 0; return { ok: true }; };
    AGENT_OPS.clearConsoleLogs = function() { consoleLogs.length = 0; return { ok: true }; };
    AGENT_OPS.clearMutationLog = function() { mutationLog.length = 0; return { ok: true }; };
    AGENT_OPS.clearDialogLog = function() { dialogLog.length = 0; return { ok: true }; };
    // Add a route rule. `rule` is an object: { pattern, match_type, method,
    // action ('block'|'fulfill'|'delay'), status, status_text, headers,
    // body, content_type, delay_ms, times }. Returns the assigned id.
    AGENT_OPS.addRoute = function(rule) {
        // The captured parse, not the page's: page script could rewrite every rule the agent adds.
        if (typeof rule === 'string') { try { rule = PRISTINE_PARSE(rule); } catch (e) { return { ok: false, error: 'invalid rule JSON' }; } }
        rule = ownCopy(rule);
        if (!rule.pattern) return { ok: false, error: 'route rule requires a pattern' };
        var matchType = rule.match_type || 'substring';
        if (matchType !== 'substring' && matchType !== 'exact' && matchType !== 'regex' && matchType !== 'glob') {
            return { ok: false, error: 'unknown match_type: ' + matchType };
        }
        var action = rule.action || 'fulfill';
        if (action !== 'fulfill' && action !== 'block' && action !== 'delay') {
            return { ok: false, error: 'unknown route action: ' + action };
        }
        // An invalid regex would otherwise be stored and silently never match.
        if (matchType === 'regex') {
            try { new REGEXP_CTOR('' + rule.pattern); } catch (e) {
                return { ok: false, error: 'invalid regex pattern: ' + e.message };
            }
            var hangs;
            try { hangs = regexHangRisk('' + rule.pattern); } catch (e) { hangs = 'this pattern'; }
            if (hangs) {
                return { ok: false, error: 'regex pattern can hang the page: ' + hangs + ' repeats a group'
                    + ' that can match the same text in more than one way, and JavaScript has no regex timeout.'
                    + ' Make each repetition match in one way only (a separator it cannot take itself, as in'
                    + ' (/\\w+)+, or no inner quantifier), or use match_type glob or substring.' };
            }
        }
        if (rule.headers !== undefined && rule.headers !== null
            && (typeof rule.headers !== 'object' || Array.isArray(rule.headers))) {
            return { ok: false, error: 'route rule headers must be an object' };
        }
        if ((rule.action || 'fulfill') === 'fulfill' && typeof rule.status === 'number'
            && (rule.status !== Math.floor(rule.status) || rule.status < 200 || rule.status > 599)) {
            return { ok: false, error: 'fulfill status must be an integer in 200-599 (a Response cannot carry ' + rule.status + ')' };
        }
        var r = {
            id: ++routeCounter,
            pattern: '' + rule.pattern,
            match_type: rule.match_type || 'substring',
            method: rule.method || null,
            action: rule.action || 'fulfill',
            status: typeof rule.status === 'number' ? rule.status : 200,
            status_text: rule.status_text || '',
            headers: rule.headers || {},
            body: (rule.body === undefined || rule.body === null) ? '' : rule.body,
            content_type: rule.content_type || 'application/json',
            delay_ms: clampTimerDelay(rule.delay_ms),
            times: typeof rule.times === 'number' ? rule.times : 0,
            triggered: 0,
        };
        if (r.action === 'fulfill' && canBuildResponses()) {
            // Build the mock once now: one the Response constructor rejects (an invalid
            // header name or value, a status text that is not a ByteString) would make every
            // matching fetch reject — and an IPC fetch that rejects is retried FOR REAL.
            try { buildMockResponse(r); } catch (e) {
                --routeCounter;
                return { ok: false, error: 'this fulfill rule\'s mock response cannot be built: ' + e };
            }
        }
        var hitsIpc = routeMatchesIpc(r);
        var unparsable = r.action === 'fulfill' ? ipcMockUnparsable(r) : null;
        if (unparsable && hitsIpc) {
            --routeCounter;
            return { ok: false, error: 'this fulfill rule matches Tauri IPC URLs, but ' + unparsable
                + ' Tauri would then treat the IPC fetch as failed, run the REAL command over'
                + ' postMessage, and keep the page on postMessage IPC until it reloads. Give a JSON'
                + ' body (e.g. "\\"text\\"" or an object), use content_type "text/plain", or narrow'
                + ' the pattern so it does not match Tauri IPC URLs (http(s)://ipc.localhost, ipc://localhost).' };
        }
        routeRules[routeRules.length] = r; // not .push (see appendCapped)
        // A copy: the stored rule was validated, and page script must not edit it afterwards.
        var out = { ok: true, id: r.id, rule: cloneJson(r) };
        if (hitsIpc) out.ipc_note = routeIpcNote(r);
        // A command-specific pattern that does not name the IPC host (`greet`, `*/greet`)
        // can still match a Tauri IPC URL. Such a call never gets this mock (Tauri would run
        // the real command over postMessage): it gets an error from Victauri instead.
        else if (unparsable) {
            out.ipc_warning = 'If this rule matches a Tauri IPC call, Tauri could not read the mock: '
                + unparsable + ' Such a call is answered with a Tauri error from Victauri instead'
                + ' (logged `mock_unreadable`), so the real command does not run.';
        }
        return out;
    };
    AGENT_OPS.clearRoute = function(id) {
        var before = routeRules.length;
        var kept = [];
        for (var i = 0; i < routeRules.length; i++) if (routeRules[i].id !== id) kept[kept.length] = routeRules[i];
        routeRules = kept;
        return { ok: true, removed: before - routeRules.length };
    };
    AGENT_OPS.clearRoutes = function() {
        var n = routeRules.length;
        routeRules = [];
        return { ok: true, removed: n };
    };
    AGENT_OPS.setDialogAutoResponse = function(type, action, text) {
        dialogAutoResponses[type] = { action: action, text: text };
        return { ok: true };
    };
    // Animation scrub + sweep recorder (R5B-SCRUBKEY1). Agent-only: from page script,
    // `installSweepRecorder` superseded the agent's armed recorder, `readSweep(true)` erased what
    // it had recorded, and `scrubPrepare` / `scrubSeek` / `scrubRestore` paused, moved and
    // resumed the page's animations under the agent. `listAnimations` stays public (read-only).
    ASSIGN(AGENT_OPS, {
        // ── Deterministic animation scrubbing ────────────────────────────────
        // Pause the target's WAAPI animations and hold state across calls so the
        // Rust side can seek to evenly-spaced progress points and capture a
        // jank-free frame at each. The paused+seeked frame is frozen, so the
        // (slow) native screenshot has nothing to race — this is why scrubbing
        // beats real-time capture for fast animations.
        scrubPrepare: function(selector) {
            var el = selector ? document.querySelector(selector) : null;
            if (!el) {
                var all = document.getAnimations ? document.getAnimations() : [];
                for (var i = 0; i < all.length; i++) {
                    if (all[i].effect && all[i].effect.target) { el = all[i].effect.target; break; }
                }
            }
            if (!el) {
                return Promise.resolve({ error: 'no target: selector matched nothing and no '
                    + 'animation is currently running. Trigger the animation, then scrub.',
                    anim_count: 0 });
            }
            var anims = (el.getAnimations ? el.getAnimations() : []).filter(function(a) {
                var ct = (a.effect && a.effect.getComputedTiming) ? a.effect.getComputedTiming() : null;
                return ct && isFinite(ct.endTime) && ct.endTime > 0;
            });
            if (!anims.length) {
                return Promise.resolve({ error: 'no seekable WAAPI animation on target — it may '
                    + 'be JS/requestAnimationFrame-driven (not seekable). Use animation sample '
                    + 'instead.', anim_count: 0 });
            }
            var ends = anims.map(function(a) { return a.effect.getComputedTiming().endTime; });
            var duration = Math.max.apply(null, ends);
            // What restore must put back. Read before pausing, or every animation reads paused.
            var saved = anims.map(function(a) { return { time: a.currentTime, state: a.playState }; });
            // A scrub replaced without a restore leaves that target to the page again.
            if (scrubState) releaseScrubEvents(scrubState.hold);
            var hold = holdScrubEvents(el);
            anims.forEach(function(a) { try { a.pause(); } catch (e) {} });
            scrubState = { el: el, anims: anims, ends: ends, saved: saved, duration: duration, hold: hold };
            return Promise.all(anims.map(function(a) { return a.ready.catch(function(){}); }))
                .then(function() {
                    var b = el.getBoundingClientRect();
                    return { prepared: true, anim_count: anims.length, duration: duration,
                        target: { tag: el.tagName.toLowerCase(), id: el.id || null,
                            rect: { x: Math.round(b.x), y: Math.round(b.y),
                                    w: Math.round(b.width), h: Math.round(b.height) } } };
                });
        },

        scrubSeek: function(progress) {
            var S = scrubState;
            if (!S) return Promise.resolve({ error: 'not prepared — scrubPrepare first' });
            var t = progress * S.duration;
            for (var i = 0; i < S.anims.length; i++) {
                // Just short of the end: reaching it enters the "after" phase, which fires the app's
                // `animationend` (toast removal, route transitions) for a scrub.
                try { S.anims[i].currentTime = Math.max(0, Math.min(t, S.ends[i] - 0.01)); } catch (e) {}
            }
            return Promise.all(S.anims.map(function(a) { return a.ready.catch(function(){}); }))
                .then(function() {
                    return new Promise(function(res) {
                        requestAnimationFrame(function() { requestAnimationFrame(res); });
                        // rAF does not fire in a hidden or minimized window.
                        SET_TIMEOUT(res, 250);
                    });
                })
                .then(function() {
                    var el = S.el, b = el.getBoundingClientRect(), cs = window.getComputedStyle(el);
                    var tf = (function(s) {
                        if (!s || s.indexOf('matrix') !== 0) return { tx: 0, ty: 0, sx: 1, sy: 1 };
                        // Strip the `matrix3d(` / `matrix(` prefix first — otherwise the
                        // '3' of "matrix3d" is parsed as the first number.
                        var m = s.replace(/^matrix(3d)?\(/, '').match(/-?[\d.eE+-]+/g);
                        if (!m) return { tx: 0, ty: 0, sx: 1, sy: 1 };
                        m = m.map(Number);
                        return m.length === 6
                            ? { tx: m[4], ty: m[5], sx: m[0], sy: m[3] }
                            : { tx: m[12], ty: m[13], sx: m[0], sy: m[5] };
                    })(cs.transform);
                    var r2 = function(n) { return Math.round(n * 100) / 100; };
                    return { progress: progress, t: r2(t),
                        rect: { x: r2(b.x), y: r2(b.y), w: Math.round(b.width), h: Math.round(b.height) },
                        transform: { tx: r2(tf.tx), ty: r2(tf.ty), sx: tf.sx, sy: tf.sy },
                        opacity: parseFloat(cs.opacity) };
                });
        },

        scrubRestore: function(resume) {
            var S = scrubState;
            if (!S) return { restored: false };
            // Back to the recorded time and state. A bare `play()` after seeking to the end rewinds
            // and REPLAYS the animation, and starts one the app had paused.
            if (resume) {
                S.anims.forEach(function(a, i) {
                    try {
                        var was = S.saved[i];
                        if (was.time !== null) a.currentTime = was.time;
                        if (was.state === 'running') a.play();
                        else if (was.state === 'finished') a.finish();
                    } catch (e) {}
                });
            }
            releaseScrubEvents(S.hold);
            scrubState = null;
            return { restored: true, resumed: !!resume };
        },

        // ── Real-time motion + jank recorder ─────────────────────────────────
        // Arm a requestAnimationFrame watcher that samples the target's geometry
        // every frame while it animates. Decoupled from the (blocking) eval call
        // so event-triggered sweeps are catchable: arm it, trigger the sweep,
        // then read back the measured curve + dropped-frame (jank) stats.
        installSweepRecorder: function(selector) {
            // The rAF loop stops itself once nobody has armed or read the recorder
            // for SWEEP_IDLE_STOP_MS, so an armed-and-forgotten recorder does not
            // run getComputedStyle every frame for the life of the page.
            var SWEEP_IDLE_STOP_MS = 60000;
            // A bad selector would throw in every frame: the first throw killed the loop, reported
            // as an app `[uncaught]` error, with the recorder still claiming to be armed.
            if (selector) {
                try { document.querySelector(selector); } catch (e) {
                    return { error: 'invalid selector: ' + selector + ' — ' + (e && e.message) };
                }
            }
            var R = (sweepState = { sel: selector || null,
                sessions: [], cur: null, touched: performance.now(), stopped: false,
                idle_stop_ms: SWEEP_IDLE_STOP_MS });
            var matrix = function(el) {
                var s = getComputedStyle(el).transform;
                if (!s || s.indexOf('matrix') !== 0) return { tx: 0, ty: 0, sx: 1 };
                var m = s.replace(/^matrix(3d)?\(/, '').match(/-?[\d.eE+-]+/g);
                if (!m) return { tx: 0, ty: 0, sx: 1 };
                m = m.map(Number);
                return m.length === 6 ? { tx: m[4], ty: m[5], sx: m[0] }
                                      : { tx: m[12], ty: m[13], sx: m[0] };
            };
            var pick = function() {
                if (R.sel) return document.querySelector(R.sel);
                var list = document.getAnimations ? document.getAnimations() : [];
                for (var i = 0; i < list.length; i++) {
                    if (list[i].playState === 'running' && list[i].effect && list[i].effect.target) {
                        return list[i].effect.target;
                    }
                }
                return null;
            };
            var tick = function() {
                // Stop if a newer recorder superseded this one.
                if (sweepState !== R || R.disarmed) return;
                if (performance.now() - R.touched > SWEEP_IDLE_STOP_MS) {
                    if (R.cur) {
                        R.sessions.push(R.cur);
                        if (R.sessions.length > 10) R.sessions.shift();
                        R.cur = null;
                    }
                    R.stopped = true;
                    return;
                }
                var el = pick();
                var anims = (el && el.getAnimations) ? el.getAnimations() : [];
                var running = anims.some(function(a) { return a.playState === 'running'; });
                if (running && !R.cur) {
                    var e = anims[0] && anims[0].effect;
                    R.cur = { t0: performance.now(), samples: [],
                        timing: (e && e.getTiming) ? e.getTiming() : {},
                        keyframes: (function() {
                            try { return (e && e.getKeyframes) ? e.getKeyframes() : []; }
                            catch (_) { return []; }
                        })() };
                }
                if (R.cur && el) {
                    var b = el.getBoundingClientRect(), tf = matrix(el);
                    R.cur.samples.push({ t: performance.now() - R.cur.t0,
                        x: b.x, y: b.y, w: b.width, h: b.height,
                        tx: tf.tx, ty: tf.ty, sx: tf.sx,
                        opacity: parseFloat(getComputedStyle(el).opacity) });
                    if (R.cur.samples.length > 2000) R.cur.samples.shift();
                    if (!running) {
                        R.sessions.push(R.cur);
                        if (R.sessions.length > 10) R.sessions.shift();
                        R.cur = null;
                    }
                }
                requestAnimationFrame(tick);
            };
            requestAnimationFrame(tick);
            return { installed: true, selector: R.sel };
        },

        // Stop an armed recorder; what it captured stays readable.
        disarmSweep: function() {
            var R = sweepState;
            if (!R || R.stopped) return { disarmed: false };
            if (R.cur) {
                R.sessions.push(R.cur);
                if (R.sessions.length > 10) R.sessions.shift();
                R.cur = null;
            }
            R.stopped = true;
            R.disarmed = true;
            return { disarmed: true };
        },

        readSweep: function(clear) {
            var R = sweepState;
            if (!R) {
                return { error: 'no recorder armed — call sample with record=true first, then '
                    + 'trigger the animation' };
            }
            // A read counts as activity: it keeps a live recorder from idling out.
            R.touched = performance.now();
            var r2 = function(n) { return Math.round(n * 100) / 100; };
            var out = R.sessions.map(function(s) {
                var f = s.samples, gaps = [];
                for (var i = 1; i < f.length; i++) gaps.push(f[i].t - f[i - 1].t);
                var jank = gaps.filter(function(g) { return g > 25; }).length;
                var maxGap = gaps.length ? Math.max.apply(null, gaps) : 0;
                return {
                    measured_duration_ms: f.length ? r2(f[f.length - 1].t) : 0,
                    declared: { duration: s.timing.duration, easing: s.timing.easing,
                                delay: s.timing.delay },
                    frames: f.length, jank_frames: jank, max_frame_gap_ms: r2(maxGap),
                    start: f.length ? { x: r2(f[0].x), tx: r2(f[0].tx), opacity: f[0].opacity } : null,
                    end: f.length ? { x: r2(f[f.length - 1].x), tx: r2(f[f.length - 1].tx),
                                      opacity: f[f.length - 1].opacity } : null,
                    keyframes: s.keyframes,
                    curve: f.map(function(p) {
                        return { t: r2(p.t), x: r2(p.x), tx: r2(p.tx), op: p.opacity };
                    })
                };
            });
            var active = !!R.cur;
            if (clear) R.sessions = [];
            var res = { armed: !R.stopped, selector: R.sel, recording_active: active,
                     session_count: out.length, sessions: out };
            if (R.disarmed) {
                res.note = 'recorder disarmed; re-arm with record=true to record again';
            } else if (R.stopped) {
                res.note = 'recorder stopped after ' + (R.idle_stop_ms / 1000) + 's with no arm/read — '
                    + 'sessions captured before that are above; re-arm with record=true to record again';
            }
            return res;
        },
    });
    return Object.freeze(AGENT_OPS);
    })();

    // ── Accessibility Helpers ────────────────────────────────────────────────

    function describeEl(el) {
        var s = '<' + el.tagName.toLowerCase();
        if (el.id) s += ' id="' + el.id + '"';
        if (el.className && typeof el.className === 'string') {
            var cls = el.className.trim();
            if (cls) s += ' class="' + truncText(cls, 50) + '"';
        }
        s += '>';
        return s;
    }

    function parseColor(str) {
        if (!str) return null;
        var m = str.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)/);
        if (!m) return null;
        return { r: parseInt(m[1]), g: parseInt(m[2]), b: parseInt(m[3]), a: m[4] !== undefined ? parseFloat(m[4]) : 1 };
    }

    function luminance(c) {
        var rs = c.r / 255, gs = c.g / 255, bs = c.b / 255;
        var r = rs <= 0.03928 ? rs / 12.92 : Math.pow((rs + 0.055) / 1.055, 2.4);
        var g = gs <= 0.03928 ? gs / 12.92 : Math.pow((gs + 0.055) / 1.055, 2.4);
        var b = bs <= 0.03928 ? bs / 12.92 : Math.pow((bs + 0.055) / 1.055, 2.4);
        return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    }

    function contrastRatio(fg, bg) {
        var l1 = luminance(fg), l2 = luminance(bg);
        var lighter = Math.max(l1, l2), darker = Math.min(l1, l2);
        return (lighter + 0.05) / (darker + 0.05);
    }

    // ── Long Task Observer ──────────────────────────────────────────────────

    try {
        var ltObserver = new PerformanceObserver(function(list) {
            var entries = list.getEntries();
            for (var i = 0; i < entries.length; i++) {
                appendCapped(longTasks, { duration: entries[i].duration, startTime: entries[i].startTime }, CAP_LONG_TASKS);
            }
        });
        ltObserver.observe({ type: 'longtask', buffered: true });
    } catch(e) {}

    // ── Event Listener Counter ──────────────────────────────────────────────

    (function() {
        var origAdd = EventTarget.prototype.addEventListener;
        var origRemove = EventTarget.prototype.removeEventListener;
        EventTarget.prototype.addEventListener = function() {
            listenerCount++;
            return origAdd.apply(this, arguments);
        };
        EventTarget.prototype.removeEventListener = function() {
            if (listenerCount > 0) listenerCount--;
            return origRemove.apply(this, arguments);
        };
    })();

    // ── DOM Walking ──────────────────────────────────────────────────────────

    function walkDom(node, isRoot) {
        var out = [];
        walkDomInto(node, out, isRoot === true);
        return out.length ? out[0] : null;
    }
    // Appends the element for `node` to `out`. A `visibility:hidden` element is skipped but its
    // children are not: CSS lets them be `visibility:visible`, and they are then what a user sees.
    // How many `opacity:0` elements enclose the node being walked: opacity does not inherit, so a
    // child's own computed style reads 1 while nothing of it is rendered.
    var transparentDepth = 0;
    function walkDomInto(node, out, isRoot) {
        if (!node || nodeKind(node) !== 1) return;
        var style = window.getComputedStyle(node);
        var zero = style.opacity === '0';
        if (zero) transparentDepth++;
        try { walkStyledInto(node, out, isRoot, style); }
        finally { if (zero) transparentDepth--; }
    }
    function walkStyledInto(node, out, isRoot, style) {
        if (style.display === 'none') return;
        var hidden = style.visibility === 'hidden';

        if (hidden && !isRoot) {
            walkChildrenInto(node, out);
            return;
        }

        var ref_id = registerRef(node);

        var rect = elRect(node);
        var tagName = elTag(node);
        var role = elAttr(node, 'role') || inferRole(node);
        var name = accessibleName(node);

        var element = {
            ref_id: ref_id,
            tag: tagName.toLowerCase(),
            role: role,
            name: name,
            text: getDirectText(node),
            value: isPasswordInput(node) ? '[REDACTED]' : safeValue(node),
            enabled: isElEnabled(node),
            visible: !hidden,
            focusable: node.tabIndex >= 0 || ['INPUT','BUTTON','SELECT','TEXTAREA','A'].indexOf(tagName) !== -1,
            bounds: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
            children: [],
            attributes: {}
        };
        // Rendered fully transparent, but still what a click at that point hits.
        if (transparentDepth > 0) element.transparent = true;

        var interestingAttrs = ['data-testid', 'id', 'type', 'href', 'src', 'checked', 'selected'];
        for (var a = 0; a < interestingAttrs.length; a++) {
            if (elHasAttr(node, interestingAttrs[a])) {
                element.attributes[interestingAttrs[a]] = elAttr(node, interestingAttrs[a]);
            }
        }

        walkChildrenInto(node, element.children);

        // Same-origin iframe traversal: descend into accessible frame documents.
        // Cross-origin frames throw on contentDocument access — mark and skip.
        if (tagName === 'IFRAME' || tagName === 'FRAME') {
            try {
                var idoc = node.contentDocument;
                if (idoc && idoc.body) {
                    var frameChild = walkDom(idoc.body, true);
                    if (frameChild) {
                        frameChild.frame = true;
                        element.children.push(frameChild);
                    }
                } else {
                    element.attributes['cross_origin_frame'] = 'true';
                }
            } catch (e) {
                element.attributes['cross_origin_frame'] = 'true';
            }
        }

        out.push(element);
    }
    function walkChildrenInto(node, out) {
        var kids = elKids(node);
        for (var c = 0; c < kids.length; c++) walkDomInto(kids[c], out, false);
        var sr = elShadow(node);
        if (sr) {
            for (var s = 0; s < sr.children.length; s++) walkDomInto(sr.children[s], out, false);
        }
    }

    // An element's `.value` for a snapshot: a string (or null). `.value` is not always a string
    // — `<li value=3>`, `<progress>`, `<meter>` and many web components expose a number or an
    // object (possibly self-referencing) — and calling string methods on it, or serializing a
    // cyclic object, used to fail the WHOLE dom_snapshot / find_elements call.
    function safeValue(node) {
        var v;
        try { v = node.value; } catch (e) { return null; }
        if (typeof v === 'string') return v || null;
        if ((typeof v === 'number' && v === v) || typeof v === 'boolean') return v ? '' + v : null;
        return null;
    }

    function isPasswordInput(node) {
        return elTag(node) === 'INPUT' && (elAttr(node, 'type') || '').toLowerCase() === 'password';
    }

    // A page-derived string for the compact tree, always as a quoted JSON string. U+2028 /
    // U+2029 / U+0085 are escaped too: JSON leaves them raw, and a reader may treat them as a
    // line break (and so as the start of a forged `[<ref>] …` line).
    function compactStr(v) {
        return PRISTINE_STRINGIFY('' + v)
            .replace(/\u2028/g, '\\u2028').replace(/\u2029/g, '\\u2029').replace(/\u0085/g, '\\u0085');
    }

    // An attribute value for the compact tree: bare when it is one plain token (the common
    // `@submit-btn`, `type=password`, `href=/about?x=1`), otherwise a quoted JSON string — a
    // value holding a space or quote used to print unquoted and spoof further same-line fields
    // (`@note type=password`, `href=https://evil`).
    var COMPACT_TOKEN_RE = /^[A-Za-z0-9_\-.:\/#?&=%+~,]+$/;
    function compactAttr(v) {
        var s = '' + v;
        return COMPACT_TOKEN_RE.test(s) ? s : compactStr(s);
    }
    // A role or tag printed as a line's head word. Anything but a plain lowercase word (a role
    // attribute is free text) would let page markup inject a forged line or fields.
    var COMPACT_WORD_RE = /^[a-z][a-z0-9-]*$/;

    function walkDomCompact(node, depth, isRoot) {
        if (!node || nodeKind(node) !== 1) return '';
        var style = window.getComputedStyle(node);
        var zero = style.opacity === '0';
        if (zero) transparentDepth++;
        try { return walkStyledCompact(node, depth, isRoot, style); }
        finally { if (zero) transparentDepth--; }
    }
    function walkStyledCompact(node, depth, isRoot, style) {
        if (style.display === 'none') return '';
        // See walkDomInto: a hidden element is not listed, its visible children are.
        if (style.visibility === 'hidden' && isRoot !== true) return compactChildren(node, depth);

        var ref_id = registerRef(node);
        var indent = '';
        for (var d = 0; d < depth; d++) indent += '  ';

        var role = elAttr(node, 'role') || inferRole(node);
        var name = accessibleName(node) || '';
        var text = getDirectText(node) || '';
        var tag = elTag(node).toLowerCase();

        // Grammar, one line per element (every page-derived field is a bare plain token or a
        // JSON string, so markup cannot forge a line or a field):
        //   [<ref>] <head> [role=<json>] [<json name/text>] [[disabled]] [[transparent]] [value=<json>]
        //        [@<attr>] [type=<attr>] [href=<attr>]
        var line = indent + '[' + ref_id + '] ';

        var head = COMPACT_WORD_RE.test(tag) ? tag : compactStr(tag);
        if (role && role !== tag) {
            if (COMPACT_WORD_RE.test(role)) {
                line += role;
            } else {
                line += head + ' role=' + compactStr(truncText(role, 60));
            }
        } else {
            line += head;
        }

        // Page-derived strings are JSON-encoded: written raw, a newline in rendered text (an
        // RSS title, a chat message) forged whole `[<ref>] button "…"` lines in this tree and could
        // steer an agent's click onto a different element.
        if (name) {
            line += ' ' + compactStr(truncText(name, 60));
        } else if (text && text.length <= 60) {
            line += ' ' + compactStr(text);
        } else if (text) {
            line += ' ' + compactStr(truncText(text, 57) + '...');
        }

        if (!isElEnabled(node)) line += ' [disabled]';
        if (transparentDepth > 0) line += ' [transparent]';
        var value = safeValue(node);
        if (value !== null) {
            line += ' value=' + compactStr(isPasswordInput(node) ? '[REDACTED]' : truncText(value, 40));
        }

        var testId = elAttr(node, 'data-testid');
        if (testId) line += ' @' + compactAttr(testId);

        var type = elAttr(node, 'type');
        if (type && tag === 'input') line += ' type=' + compactAttr(type);

        var href = elAttr(node, 'href');
        if (href && tag === 'a') line += ' href=' + compactAttr(truncText(href, 60));

        var result = line + '\n';

        result += compactChildren(node, depth + 1);

        // Same-origin iframe traversal (see walkDom for rationale).
        if (tag === 'iframe' || tag === 'frame') {
            try {
                var idoc = node.contentDocument;
                if (idoc && idoc.body) {
                    result += indent + '  ⤷ iframe content:\n';
                    result += walkDomCompact(idoc.body, depth + 2, true);
                } else {
                    result += indent + '  ⤷ [cross-origin iframe]\n';
                }
            } catch (e) {
                result += indent + '  ⤷ [cross-origin iframe]\n';
            }
        }

        return result;
    }

    function compactChildren(node, depth) {
        var result = '';
        var kids = elKids(node);
        for (var c = 0; c < kids.length; c++) result += walkDomCompact(kids[c], depth);
        var sr = elShadow(node);
        if (sr) {
            for (var s = 0; s < sr.children.length; s++) result += walkDomCompact(sr.children[s], depth);
        }
        return result;
    }

    function inferRole(node) {
        var tag = elTag(node);
        var roles = {
            'BUTTON': 'button', 'A': 'link', 'INPUT': 'textbox',
            'SELECT': 'combobox', 'TEXTAREA': 'textbox', 'IMG': 'img',
            'NAV': 'navigation', 'MAIN': 'main', 'HEADER': 'banner',
            'FOOTER': 'contentinfo', 'ASIDE': 'complementary',
            'H1': 'heading', 'H2': 'heading', 'H3': 'heading',
            'H4': 'heading', 'H5': 'heading', 'H6': 'heading',
            'UL': 'list', 'OL': 'list', 'LI': 'listitem',
            'TABLE': 'table', 'FORM': 'form', 'DIALOG': 'dialog',
        };
        if (tag === 'INPUT') {
            var type = elAttr(node, 'type');
            if (type === 'checkbox') return 'checkbox';
            if (type === 'radio') return 'radio';
            if (type === 'range') return 'slider';
            if (type === 'submit' || type === 'button') return 'button';
        }
        return roles[tag] || null;
    }

    function getDirectText(node) {
        var text = '';
        var kids = nodeKids(node);
        for (var i = 0; i < kids.length; i++) {
            if (kids[i].nodeType === 3) text += kids[i].nodeValue;
        }
        text = text.trim();
        return text.length > 0 ? truncText(text, 200) : null;
    }

    // ── Element text and accessible name ─────────────────────────────────────

    var TEXTLESS_TAGS = { SCRIPT: true, STYLE: true, NOSCRIPT: true, TEMPLATE: true };
    function normText(s) { return s.replace(/\s+/g, ' ').trim(); }
    // The text under `node` (`textContent`, minus script and style source).
    function nodeFullText(node) {
        var out = '';
        var kids = nodeKids(node);
        for (var i = 0; i < kids.length; i++) {
            var k = kids[i];
            var kind = nodeKind(k);
            if (kind === 3) out += k.nodeValue;
            else if (kind === 1 && TEXTLESS_TAGS[elTag(k)] !== true) out += nodeFullText(k);
        }
        return out;
    }
    function pageText(root) {
        if (!root) return '';
        // A ShadowRoot has no innerText.
        var text = typeof root.innerText === 'string' ? root.innerText : nodeFullText(root);
        var els = root.querySelectorAll('*');
        for (var j = 0; j < els.length; j++) {
            var sr = elShadow(els[j]);
            if (sr) text += ' ' + pageText(sr);
            var t = elTag(els[j]);
            if (t === 'IFRAME' || t === 'FRAME') {
                try {
                    var d = els[j].contentDocument;
                    if (d && d.body) text += ' ' + pageText(d.body);
                } catch (e) { /* cross-origin */ }
            }
        }
        return text;
    }
    function cssVisibleDeep(el) {
        var view = (el.ownerDocument && el.ownerDocument.defaultView) || window;
        var cs = view.getComputedStyle(el);
        if (cs.visibility === 'hidden' || cs.visibility === 'collapse') return false;
        for (var n = el; n && nodeKind(n) === 1; n = n.parentNode) {
            if (view.getComputedStyle(n).display === 'none') return false;
        }
        return true;
    }
    // Roles named from their content (ARIA "name from content").
    var NAME_FROM_TEXT_ROLES = { button: 1, link: 1, tab: 1, menuitem: 1, menuitemcheckbox: 1, menuitemradio: 1,
        option: 1, radio: 1, checkbox: 1, switch: 1, treeitem: 1, heading: 1, cell: 1, columnheader: 1, rowheader: 1,
        tooltip: 1 };
    // The accessible name: aria-label, aria-labelledby, an associated <label>, the text of a
    // button / link / summary-like element, then title and placeholder. One computation for the
    // `name` result field, the `name` filter and both snapshots. Null when there is none.
    function accessibleName(el) {
        var aria = normText(elAttr(el, 'aria-label') || '');
        if (aria) return aria;
        var by = elAttr(el, 'aria-labelledby');
        if (by) {
            var root = el.getRootNode ? el.getRootNode() : document;
            var ids = normText(by).split(' ');
            var parts = [];
            for (var i = 0; i < ids.length; i++) {
                var ref = root.getElementById ? root.getElementById(ids[i]) : null;
                if (ref) parts[parts.length] = normText(nodeFullText(ref));
            }
            var joined = normText(parts.join(' '));
            if (joined) return joined;
        }
        var tag = elTag(el);
        var labels = null;
        try { labels = el.labels; } catch (e) { labels = null; }
        if (labels && labels.length) {
            var lt = [];
            for (var l = 0; l < labels.length; l++) lt[lt.length] = normText(nodeFullText(labels[l]));
            var lj = normText(lt.join(' '));
            if (lj) return lj;
        }
        if (tag === 'INPUT') {
            var ty = (elAttr(el, 'type') || '').toLowerCase();
            if (ty === 'submit' || ty === 'button' || ty === 'reset') {
                var iv = normText(elAttr(el, 'value') || '');
                if (iv) return iv;
            }
            if (ty === 'image' && elAttr(el, 'alt')) return elAttr(el, 'alt');
        }
        if (tag === 'IMG' && elAttr(el, 'alt')) return elAttr(el, 'alt');
        var role = elAttr(el, 'role');
        if (tag === 'BUTTON' || tag === 'A' || tag === 'SUMMARY' || (tag.length === 2 && tag.charAt(0) === 'H' && tag.charAt(1) >= '1' && tag.charAt(1) <= '6')
            || (role && NAME_FROM_TEXT_ROLES[role] === 1)) {
            var t = truncText(normText(nodeFullText(el)), 200);
            if (t) return t;
        }
        return elAttr(el, 'title') || elAttr(el, 'placeholder') || null;
    }

    // ── Input editing ────────────────────────────────────────────────────────

    var MAX_TYPE_CHARS = 20000;
    // The browser's value sanitization clears or clamps what these types cannot hold.
    var SANITIZED_INPUT_TYPES = { number: 1, date: 1, time: 1, 'datetime-local': 1, month: 1, week: 1, color: 1, range: 1 };
    var UNEDITABLE_INPUT_TYPES = { checkbox: 'click', radio: 'click', file: 'a file dialog', button: 'click', submit: 'click',
        reset: 'click', image: 'click', hidden: 'nothing: it is hidden' };
    function inputType(el) { return ('' + (el.type || 'text')).toLowerCase(); }
    function editKind(el) {
        var tag = elTag(el);
        if (tag === 'TEXTAREA') return 'text';
        if (tag === 'INPUT') return hasOwn(SANITIZED_INPUT_TYPES, inputType(el)) ? 'sanitized' : 'text';
        // jsdom has no isContentEditable; the attribute is what it reflects.
        if (typeof el.isContentEditable === 'boolean') return el.isContentEditable ? 'contenteditable' : null;
        var ce = elAttr(el, 'contenteditable');
        return ce !== null && ce.toLowerCase() !== 'false' ? 'contenteditable' : null;
    }
    function refuseEdit(el) {
        if (editKind(el) === null) {
            return { ok: false, error: 'element is not fillable (not input, textarea, or contenteditable): ' + elTag(el).toLowerCase(), hint: 'CHECK_INPUT' };
        }
        if (elTag(el) === 'INPUT' && hasOwn(UNEDITABLE_INPUT_TYPES, inputType(el))) {
            return { ok: false, error: 'element is not fillable: input type=' + inputType(el) + ' (use ' + UNEDITABLE_INPUT_TYPES[inputType(el)] + ')', hint: 'CHECK_INPUT' };
        }
        if (el.readOnly === true) return { ok: false, error: 'element is readonly: a user cannot edit it', hint: 'CHECK_INPUT' };
        return null;
    }
    // The native value setter of the element's OWN realm (a frame's textarea fails the top
    // realm's `instanceof`): React and friends shadow `value` on the instance.
    function valueSetter(el) {
        var win = (el.ownerDocument && el.ownerDocument.defaultView) || window;
        var proto = elTag(el) === 'TEXTAREA' ? win.HTMLTextAreaElement.prototype : win.HTMLInputElement.prototype;
        var desc = Object.getOwnPropertyDescriptor(proto, 'value');
        return function(v) {
            if (desc && desc.set) REFLECT_APPLY(desc.set, el, [v]);
            else el.value = v;
        };
    }
    // Did the browser refuse `asked` on a sanitizing input type? Only those are judged: a text
    // field's page handlers may legitimately reformat what is typed.
    function sanitizerRejection(el, asked) {
        if (elTag(el) !== 'INPUT' || !hasOwn(SANITIZED_INPUT_TYPES, inputType(el))) return null;
        var type = inputType(el), got = el.value, bad;
        if (type === 'color') bad = got.toLowerCase() !== asked.toLowerCase();
        else if (type === 'range') bad = Number(got) !== Number(asked);
        else bad = got === '' && asked !== '';
        if (!bad) return null;
        return { ok: false, error: 'value rejected by input type=' + type + ': ' + jsonStringLiteral(truncText(asked, 80)) + ' (the field is unchanged)', hint: 'CHECK_INPUT' };
    }
    // One user-perceived key: a surrogate pair is one character, not two key events.
    function charAtPoint(s, i) {
        var c = s.charCodeAt(i);
        if (c >= 0xD800 && c <= 0xDBFF && i + 1 < s.length) {
            var d = s.charCodeAt(i + 1);
            if (d >= 0xDC00 && d <= 0xDFFF) return s.substring(i, i + 2);
        }
        return s.charAt(i);
    }
    var KEY_CODES = { Enter: 13, Tab: 9, Escape: 27, Backspace: 8, Delete: 46, ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39,
        ArrowDown: 40, Home: 36, End: 35, PageUp: 33, PageDown: 34, Insert: 45 };
    var MOD_CODES = { Control: ['ControlLeft', 17], Shift: ['ShiftLeft', 16], Alt: ['AltLeft', 18], Meta: ['MetaLeft', 91] };
    var PUNCT_CODES = { '-': 'Minus', '_': 'Minus', '=': 'Equal', '+': 'Equal', '[': 'BracketLeft', ']': 'BracketRight',
        ';': 'Semicolon', ':': 'Semicolon', ',': 'Comma', '.': 'Period', '/': 'Slash', '?': 'Slash', '\\': 'Backslash', "'": 'Quote', '`': 'Backquote' };
    // `code` and `keyCode` too: hotkey handlers commonly read those, and an empty `code` never fired them.
    function keyInit(key, mods) {
        var code = '', keyCode = 0;
        if (key === ' ') { code = 'Space'; keyCode = 32; }
        else if (key.length === 1) {
            var up = key.toUpperCase(), c = up.charCodeAt(0);
            if (c >= 65 && c <= 90) { code = 'Key' + up; keyCode = c; }
            else if (c >= 48 && c <= 57) { code = 'Digit' + up; keyCode = c; }
            else if (hasOwn(PUNCT_CODES, key)) { code = PUNCT_CODES[key]; }
        } else if (hasOwn(KEY_CODES, key)) { code = key; keyCode = KEY_CODES[key]; }
        else if (hasOwn(MOD_CODES, key)) { code = MOD_CODES[key][0]; keyCode = MOD_CODES[key][1]; }
        else if (key.charAt(0) === 'F' && +key.substring(1) >= 1 && +key.substring(1) <= 12) { code = key; keyCode = 111 + (+key.substring(1)); }
        return { key: key, code: code, keyCode: keyCode, which: keyCode, bubbles: true, cancelable: true, composed: true,
            ctrlKey: !!(mods && mods.ctrlKey), shiftKey: !!(mods && mods.shiftKey), altKey: !!(mods && mods.altKey), metaKey: !!(mods && mods.metaKey) };
    }
    // `document.activeElement` retargets to a shadow host or a frame element; keys go to what
    // actually has focus.
    function deepActiveElement() {
        var a = document.activeElement || document.body;
        for (var g = 0; g < 32 && a; g++) {
            var inner = null;
            var sr = elShadow(a);
            if (sr && sr.activeElement) inner = sr.activeElement;
            else if (elTag(a) === 'IFRAME' || elTag(a) === 'FRAME') {
                try { inner = a.contentDocument && a.contentDocument.activeElement; } catch (e) { inner = null; }
            }
            if (!inner || inner === a) break;
            a = inner;
        }
        return a;
    }
    // The descendant of `el` that a click at its center actually hits, or null.
    function hitDescendant(el) {
        var doc = el.ownerDocument || document;
        var r = el.getBoundingClientRect();
        var hit = deepElementFromPoint(doc, r.left + r.width / 2, r.top + r.height / 2);
        return hit && hit !== el && composedContains(el, hit) ? hit : null;
    }
    // SVG and MathML elements have no `click()`.
    function activate(el) {
        if (typeof el.click === 'function') { el.click(); return; }
        var view = (el.ownerDocument && el.ownerDocument.defaultView) || window;
        el.dispatchEvent(new view.MouseEvent('click', { bubbles: true, cancelable: true, composed: true, view: view }));
    }

    // ── Console Hooking ──────────────────────────────────────────────────────

    var originalConsole = {
        log: console.log, warn: console.warn,
        error: console.error, info: console.info, debug: console.debug
    };

    var CTRL_RE = /[\x00-\x08\x0B\x0C\x0E-\x1F\x7F\x1B]/g;

    var MAX_CONSOLE_MSG = 4096;

    // String(a) throws for Object.create(null), module namespace objects, and
    // objects with a throwing toString/Symbol.toPrimitive — never let that
    // escape into the app's console call.
    function safeArgString(a) {
        try { return String(a); } catch (e) {
            try { return Object.prototype.toString.call(a); } catch (e2) { return '[unprintable]'; }
        }
    }

    // Strip control characters and cap at MAX_CONSOLE_MSG, marking what was cut. Shared by
    // console capture and uncaught-error capture (R5-JS5: the latter stored whole messages).
    function capConsoleMessage(msg, skippedArgs) {
        msg = msg.replace(CTRL_RE, '');
        if (msg.length > MAX_CONSOLE_MSG) {
            msg = truncText(msg, MAX_CONSOLE_MSG) + '…[+' + (msg.length - MAX_CONSOLE_MSG) + ' bytes truncated'
                + (skippedArgs ? ', ' + skippedArgs + ' more args' : '') + ']';
        }
        return msg;
    }

    function hookConsole(level) {
        console[level] = function() {
            // Tauri announces its permanent switch to postMessage IPC with this warning. (Page
            // script can forge it; that only reports capture as degraded — the safe direction.)
            if (level === 'warn') {
                try {
                    var first = arguments[0];
                    if (typeof first === 'string' && first.lastIndexOf(FALLBACK_WARNING_PREFIX, 0) === 0) {
                        noteIpcFallback('console_warning');
                    }
                } catch (e) {}
            }
            try {
                var msg = '';
                var skippedArgs = 0;
                for (var i = 0; i < arguments.length; i++) {
                    if (msg.length > MAX_CONSOLE_MSG) { skippedArgs = arguments.length - i; break; }
                    if (i) msg += ' ';
                    msg += safeArgString(arguments[i]);
                }
                appendCapped(consoleLogs, { level: level, message: capConsoleMessage(msg, skippedArgs), timestamp: Date.now() }, CAP_CONSOLE);
            } catch (e) {}
            // Always forward to the original method, whatever capture did.
            return originalConsole[level].apply(console, arguments);
        };
    }

    hookConsole('log');
    hookConsole('warn');
    hookConsole('error');
    hookConsole('info');
    hookConsole('debug');

    // ── Global Error Capture ────────────────────────────────────────────────

    window.addEventListener('error', function(e) {
        try {
            var msg = safeArgString(e.message || 'Unknown error');
            if (e.filename) msg += ' at ' + safeArgString(e.filename) + ':' + e.lineno + ':' + e.colno;
            appendCapped(consoleLogs, { level: 'error', message: capConsoleMessage('[uncaught] ' + msg, 0), timestamp: Date.now() }, CAP_CONSOLE);
        } catch (x) {}
    });

    window.addEventListener('unhandledrejection', function(e) {
        try {
            var r = e.reason, msg;
            if (!r) msg = 'Unhandled promise rejection';
            else {
                var m; try { m = r.message; } catch (x) { m = undefined; }
                msg = safeArgString(m || r);
            }
            appendCapped(consoleLogs, { level: 'error', message: capConsoleMessage('[unhandled rejection] ' + msg, 0), timestamp: Date.now() }, CAP_CONSOLE);
        } catch (x) {}
    });

    // ── Interaction Observer (for record mode) ────────────────────────────────

    // CSS escaping for recorded selectors (the codegen decoder in victauri-core parses exactly
    // these forms). cssString: the body of a double-quoted CSS string — `\` -> `\\`, `"` -> `\"`,
    // every char < U+0020, U+007F, U+2028 and U+2029 -> `\` + lowercase hex + ONE space (a
    // newline is `\a `); everything else literal. cssIdent: CSS.escape() (CSSOM "serialize an
    // identifier"), implemented here so it cannot be replaced by page script.
    function hexEscape(code) { return '\\' + code.toString(16) + ' '; }
    function cssString(s) {
        s = '' + s;
        var out = '';
        for (var i = 0; i < s.length; i++) {
            var c = s.charCodeAt(i);
            if (c < 0x20 || c === 0x7F || c === 0x2028 || c === 0x2029) out += hexEscape(c);
            else if (c === 0x5C) out += '\\\\';
            else if (c === 0x22) out += '\\"';
            else out += s.charAt(i);
        }
        return out;
    }
    function cssIdent(s) {
        s = '' + s;
        var out = '';
        var first = s.charCodeAt(0);
        for (var i = 0; i < s.length; i++) {
            var c = s.charCodeAt(i);
            if (c === 0) out += '\uFFFD';
            else if ((c >= 0x1 && c <= 0x1F) || c === 0x7F
                || (i === 0 && c >= 0x30 && c <= 0x39)
                || (i === 1 && c >= 0x30 && c <= 0x39 && first === 0x2D)) out += hexEscape(c);
            else if (i === 0 && c === 0x2D && s.length === 1) out += '\\-';
            else if (c >= 0x80 || c === 0x2D || c === 0x5F || (c >= 0x30 && c <= 0x39)
                || (c >= 0x41 && c <= 0x5A) || (c >= 0x61 && c <= 0x7A)) out += s.charAt(i);
            else out += '\\' + s.charAt(i);
        }
        return out;
    }

    function bestSelector(el) {
        if (el.dataset && el.dataset.testid) return '[data-testid="' + cssString(el.dataset.testid) + '"]';
        if (el.id) return '#' + cssIdent(el.id);
        if (el.getAttribute && el.getAttribute('role')) {
            var role = el.getAttribute('role');
            var text = truncText((el.textContent || '').trim(), 50);
            if (text) return '[role="' + cssString(role) + '"]:has-text("' + cssString(text) + '")';
            return '[role="' + cssString(role) + '"]';
        }
        var tag = (el.tagName || 'div').toLowerCase();
        var text = truncText((el.textContent || '').trim(), 50);
        if (text && ['button', 'a', 'label', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'span'].indexOf(tag) !== -1) {
            return tag + ':has-text("' + cssString(text) + '")';
        }
        if (typeof el.name === 'string' && el.name) return tag + '[name="' + cssString(el.name) + '"]';
        if (el.className && typeof el.className === 'string') {
            var classes = el.className.trim().split(/\s+/).slice(0, 2);
            if (classes[0]) return tag + '.' + classes.map(cssIdent).join('.');
        }
        return tag;
    }

    function pushInteraction(action, el, value) {
        appendCapped(interactionLog, {
            type: 'dom_interaction',
            action: action,
            selector: bestSelector(el),
            value: value ? truncText(value, MAX_CONSOLE_MSG) : null,
            timestamp: Date.now()
        }, CAP_INTERACTION);
    }

    document.addEventListener('click', function(e) {
        if (e.isTrusted && e.target) pushInteraction('click', e.target, null);
    }, true);

    document.addEventListener('dblclick', function(e) {
        if (e.isTrusted && e.target) pushInteraction('double_click', e.target, null);
    }, true);

    document.addEventListener('change', function(e) {
        if (!e.isTrusted || !e.target) return;
        var el = e.target;
        var tag = (el.tagName || '').toLowerCase();
        if (tag === 'select') {
            pushInteraction('select', el, el.value);
        } else if (tag === 'input' || tag === 'textarea') {
            var isPassword = tag === 'input' && el.type === 'password';
            pushInteraction('fill', el, isPassword ? '[REDACTED]' : el.value);
        }
    }, true);

    document.addEventListener('keydown', function(e) {
        if (!e.isTrusted) return;
        if (['Enter', 'Escape', 'Tab', 'Backspace', 'Delete', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'].indexOf(e.key) !== -1) {
            pushInteraction('key_press', e.target || document.body, e.key);
        }
    }, true);

    // ── Mutation Observer (deferred) ─────────────────────────────────────────

    var mutationBatchCount = 0;
    var mutationBatchTimer = null;
    var __mutationObserver = null;

    function startMutationObserver() {
        if (!document.documentElement) return false;
        __mutationObserver = new MutationObserver(function(mutations) {
            mutationBatchCount += mutations.length;
            if (!mutationBatchTimer) {
                mutationBatchTimer = setTimeout(function() {
                    appendCapped(mutationLog, { count: mutationBatchCount, timestamp: Date.now() }, CAP_MUTATION);
                    mutationBatchCount = 0;
                    mutationBatchTimer = null;
                }, 100);
            }
        });
        __mutationObserver.observe(document.documentElement, {
            childList: true, subtree: true, attributes: true, characterData: true,
        });
        return true;
    }

    if (!startMutationObserver()) {
        document.addEventListener('DOMContentLoaded', startMutationObserver);
    }

    // IPC logging is derived from the network log: Tauri 2.0 sends all IPC
    // via fetch to http://ipc.localhost/<command>. The fetch interceptor below
    // captures these, and getIpcLog() filters them from networkLog. This avoids
    // the need to patch __TAURI_INTERNALS__.invoke, which Tauri freezes with
    // configurable:false, writable:false.

    // ── Network Interception ─────────────────────────────────────────────────

    // IPC bodies are retained in networkLog (up to CAP_NETWORK entries), so bound
    // what one entry may hold: bodies over MAX_IPC_BODY chars, and non-JSON/binary
    // payloads (a raw `tauri::ipc::Response` is application/octet-stream), are
    // replaced by a size marker instead of being read and parsed.
    var MAX_IPC_BODY = 65536;
    function bodyOmitted(size, contentType) {
        return '[body omitted: ' + (size >= 0 ? size + ' bytes' : 'unknown size')
            + (contentType ? ', ' + contentType : '') + ']';
    }
    function ipcArgSize(body) {
        if (body === undefined || body === null) return 0;
        if (typeof body === 'string') {
            var t = body.trim();
            return (t === '' || t === '{}') ? 0 : body.length;
        }
        if (typeof body.byteLength === 'number') return body.byteLength; // ArrayBuffer / typed array
        if (typeof body.size === 'number') return body.size; // Blob
        return 1; // present but unmeasurable (FormData, stream): non-zero = "has args"
    }
    function isTextualContentType(ct) {
        if (!ct) return true; // unknown: let the size bound decide
        ct = ct.toLowerCase();
        return ct.indexOf('json') !== -1 || ct.indexOf('text/') === 0;
    }

    (function interceptNetwork() {
        // fetch
        var origFetch = window.fetch;
        if (origFetch) {
            window.fetch = function(input, init) {
                // Arguments pass through exactly as given (R5B-FETCH0): `fetch()` with no input
                // must reject natively, not fetch the string "undefined"; `fetch(x)` must not
                // grow an explicit `undefined` init.
                var argc = arguments.length;
                if (argc === 0) return REFLECT_APPLY(origFetch, this, []);
                function fetchArgs(first) { return argc === 1 ? [first] : [first, init]; }
                // Log exactly the request fetch will make (R4-JS3). A look-alike object's own
                // `url`/`method` used to be logged although fetch requests `String(input)`, so a
                // page could plant fake IPC entries (a "quit_app" call that never happened).
                // A non-Request input is converted ONCE and fetch is handed that string, so a
                // stateful `toString` cannot make the log and the request differ either.
                var url, method, fetchInput = input;
                try {
                    var isRequest = false;
                    if (REQUEST_URL_GET && input !== null && (typeof input === 'object' || typeof input === 'function')) {
                        try {
                            url = REFLECT_APPLY(REQUEST_URL_GET, input, []);
                            method = REFLECT_APPLY(REQUEST_METHOD_GET, input, []);
                            isRequest = true;
                        } catch (e) { isRequest = false; }
                    }
                    if (!isRequest) {
                        url = toStringExact(input);
                        method = 'GET';
                        fetchInput = url;
                    }
                    var initMethod = (init !== undefined && init !== null) ? init.method : undefined;
                    if (initMethod !== undefined) method = toStringExact(initMethod);
                } catch (e) {
                    // fetch itself rejects this input: let it, with nothing logged.
                    return REFLECT_APPLY(origFetch, this, fetchArgs(input));
                }
                var id = ++networkCounter;
                var isIpc = isIpcUrl(url);
                var isVictauriInternal = isVictauriInternalUrl(url);
                var entry = nullProto({ id: id, method: method.toUpperCase(), url: loggedUrl(url), timestamp: Date.now(), status: 'pending', duration_ms: null });

                if (isIpc) ipcCapture.fetch_seen = true;
                if (isIpc && !isVictauriInternal) {
                    ipcCapture.fetch_calls++;
                    // Tauri's callback id for this call: if the custom protocol fails, Tauri
                    // retries the SAME message over postMessage, and this attempt is superseded.
                    var cbHeader = initHeader(init, 'Tauri-Callback');
                    if (cbHeader !== null && cbHeader !== undefined) {
                        var cbNum = +cbHeader;
                        if (cbNum === cbNum) entry.ipc_callback = cbNum; // not NaN
                    }
                    var reqBody = init ? init.body : null;
                    // Size of the IPC request body as sent (0 for none / `{}`), so a
                    // consumer can tell whether a call carried arguments even when they
                    // were not captured (oversized or non-JSON/binary body).
                    try { entry.arg_size_bytes = ipcArgSize(reqBody); } catch (e) { entry.arg_size_bytes = 0; }
                    if (reqBody && window.__VICTAURI__._captureIpcBodies !== false) {
                        try {
                            if (typeof reqBody === 'string') {
                                if (reqBody.length > MAX_IPC_BODY) {
                                    entry.request_args = bodyOmitted(reqBody.length, null);
                                } else {
                                    entry.request_args = PRISTINE_PARSE(reqBody);
                                }
                            }
                        } catch(e) {}
                    }
                }

                if (!isVictauriInternal) {
                    appendCapped(networkLog, entry, CAP_NETWORK);
                }

                var self = this;
                function flushIpcWaiters() {
                    for (var w = ipcWaiters.length - 1; w >= 0; w--) { ipcWaiters[w](); }
                    ipcWaiters.length = 0;
                }

                // Phase 1: apply a matching route rule (block / fulfill / delay).
                var route = matchRoute(url, method);
                if (route) {
                    recordRouteMatch(route, url, method);
                    entry.route_rule = route.id;
                    if (route.action === 'block') {
                        entry.status = 'blocked';
                        entry.blocked = true;
                        entry.duration_ms = Date.now() - entry.timestamp;
                        if (isIpc) flushIpcWaiters();
                        return Promise.reject(new TypeError('victauri: request blocked by route #' + route.id + ' (' + url + ')'));
                    }
                    if (route.action === 'fulfill') {
                        var makeResp = function() {
                            // An IPC call must never get a mock Tauri cannot read: Tauri would
                            // treat the fetch as failed and run the REAL command over postMessage
                            // (and stay on postMessage). `add` refuses rules that name the IPC
                            // host; a broader pattern (`greet`, `*/greet`) lands here, and the
                            // call gets a Tauri error from Victauri instead — fail closed.
                            var unreadable = isIpc ? ipcMockUnparsable(route) : null;
                            var built = unreadable === null ? buildMockResponse(route) : buildMockResponse({
                                id: route.id, status: 200, status_text: '', content_type: 'text/plain',
                                headers: { 'Tauri-Response': 'error' },
                                body: 'victauri: route #' + route.id + ' would answer this IPC call with a mock Tauri cannot read ('
                                    + unreadable + ') Victauri answered with this error instead; the real command did not run.',
                            });
                            var resp = built.resp, bodyStr = built.body;
                            entry.status = respField(resp, RESPONSE_STATUS_GET, 'status');
                            entry.status_text = respField(resp, RESPONSE_STATUS_TEXT_GET, 'statusText');
                            entry.mocked = true;
                            if (unreadable !== null) entry.mock_unreadable = unreadable;
                            if (route.delay_ms > 0) entry.route_delay_ms = route.delay_ms;
                            entry.duration_ms = Date.now() - entry.timestamp;
                            if (isIpc) {
                                // Log the outcome the page actually gets, by Tauri's own reading
                                // of THIS response (ipc-protocol.js): it resolves only when
                                // `headers.get('Tauri-Response') === 'ok'`, and takes the body as
                                // JSON / text / an ArrayBuffer by `content-type` up to the first
                                // comma. Read through the captured getters, not page code.
                                entry.ipc_response = respHeader(resp, 'Tauri-Response') === 'ok' ? 'ok' : 'error';
                                var ctFull = respHeader(resp, 'content-type') || '';
                                var ctHead = tauriContentTypeHead(resp);
                                if (bodyStr === null) entry.response_body = null;
                                else if (bodyStr.length > MAX_IPC_BODY) entry.response_body = bodyOmitted(bodyStr.length, null);
                                else if (ctHead === 'application/json') { try { entry.response_body = PRISTINE_PARSE(bodyStr); } catch (e) { entry.response_body = bodyStr; } }
                                else if (ctHead === 'text/plain') entry.response_body = bodyStr;
                                else entry.response_body = bodyOmitted(bodyStr.length, ctFull || null); // the app gets an ArrayBuffer
                                flushIpcWaiters();
                            }
                            return resp;
                        };
                        // Build the Response inside the promise chain so a construction
                        // error becomes a rejection (as a real fetch failure would), never
                        // a synchronous throw out of fetch().
                        var waitP = route.delay_ms > 0
                            ? routeDelay(route.delay_ms)
                            : Promise.resolve();
                        return waitP.then(makeResp).catch(function(err) {
                            if (entry.status === 'aborted') throw err;
                            entry.status = 'error';
                            entry.error = String(err);
                            entry.duration_ms = Date.now() - entry.timestamp;
                            if (isIpc) flushIpcWaiters();
                            throw err;
                        });
                    }
                    if (route.action === 'delay' && route.delay_ms > 0) {
                        entry.route_delay_ms = route.delay_ms;
                        return routeDelay(route.delay_ms).then(doRealFetch);
                    }
                }
                return doRealFetch();

                // A route delay stands in for time on the network, so the request's AbortSignal
                // ends it as it would end a real request (cancelling a slow request is what delay
                // rules are for). Native fetch checks the signal itself once it starts.
                function routeDelay(ms) {
                    var signal = null;
                    try {
                        signal = (init && init.signal) || (fetchInput && typeof fetchInput === 'object' ? fetchInput.signal : null) || null;
                    } catch (e) { signal = null; }
                    return new Promise(function(resolve, reject) {
                        function aborted() {
                            CLEAR_TIMEOUT(timer);
                            entry.status = 'aborted';
                            entry.duration_ms = Date.now() - entry.timestamp;
                            if (isIpc) flushIpcWaiters();
                            reject((signal && signal.reason) || new DOMException('The operation was aborted.', 'AbortError'));
                        }
                        // Once the delay is over the signal is the real request's business: an abort
                        // after a finished request must not rewrite its entry.
                        var timer = SET_TIMEOUT(function() {
                            if (signal) { try { REFLECT_APPLY(REMOVE_LISTENER, signal, ['abort', aborted]); } catch (e) {} }
                            resolve();
                        }, ms);
                        if (!signal) return;
                        if (signal.aborted) { aborted(); return; }
                        try { REFLECT_APPLY(ADD_LISTENER, signal, ['abort', aborted]); } catch (e) {}
                    });
                }

                function doRealFetch() {
                    return REFLECT_APPLY(origFetch, self, fetchArgs(fetchInput)).then(function(response) {
                        // Native getters: page script cannot reshape what the real response said.
                        entry.status = respField(response, RESPONSE_STATUS_GET, 'status');
                        entry.status_text = respField(response, RESPONSE_STATUS_TEXT_GET, 'statusText');
                        entry.duration_ms = Date.now() - entry.timestamp;

                        if (isIpc) {
                            // Capture Tauri's command-outcome signal. The HTTP status is 200
                            // for BOTH a successful command AND a failed/"not found" one — the
                            // real Ok/Err result is carried in the `Tauri-Response` header
                            // ('ok' | 'error'; absent on a non-POST answer, which ran nothing).
                            entry.ipc_response = respHeader(response, 'Tauri-Response');
                            var ct = respHeader(response, 'Content-Type'), clen = -1;
                            var cl = respHeader(response, 'Content-Length');
                            if (cl !== null && cl !== '' && isFinite(Number(cl))) clen = Number(cl);
                            if (window.__VICTAURI__._captureIpcBodies !== false && !isTextualContentType(ct)) {
                                // Binary (e.g. raw tauri::ipc::Response bytes): never read it.
                                entry.response_body = bodyOmitted(clen, ct);
                                flushIpcWaiters();
                            } else if (window.__VICTAURI__._captureIpcBodies !== false && clen > MAX_IPC_BODY) {
                                entry.response_body = bodyOmitted(clen, null);
                                flushIpcWaiters();
                            } else if (window.__VICTAURI__._captureIpcBodies !== false) {
                                respTextCopy(response).then(function(text) {
                                    if (text.length > MAX_IPC_BODY) {
                                        entry.response_body = bodyOmitted(text.length, null);
                                        return;
                                    }
                                    try { entry.response_body = PRISTINE_PARSE(text); } catch(e) { entry.response_body = text; }
                                }).catch(function() {}).then(function() {
                                    flushIpcWaiters();
                                });
                            } else {
                                flushIpcWaiters();
                            }
                        }

                        return response;
                    }, function(err) {
                        entry.status = 'error';
                        entry.error = String(err);
                        entry.duration_ms = Date.now() - entry.timestamp;
                        flushIpcWaiters();
                        throw err;
                    });
                }
            };
        }

        // XMLHttpRequest
        var origOpen = XMLHttpRequest.prototype.open;
        var origSend = XMLHttpRequest.prototype.send;
        var origAbort = XMLHttpRequest.prototype.abort;
        var ADD_LISTENER = EventTarget.prototype.addEventListener;
        var REMOVE_LISTENER = EventTarget.prototype.removeEventListener;
        var DISPATCH_EVENT = EventTarget.prototype.dispatchEvent;
        // Per-XHR state in a closure WeakMap — not an expando on the XHR, which page script
        // could overwrite to forge the entry `send()` logs (R4-JS3): the {method, url} `open()`
        // really received, and the log entry of the request currently in flight. The WeakMap
        // methods are captured before page script runs.
        //
        // The bridge's listeners are attached ONCE per XHR and always update the CURRENT entry
        // (R5B-XHR1). Attaching them on every send() piled them up on a reused XHR, each closing
        // over its own send's entry, so every later request rewrote the earlier entries.
        var XHR_GET_HEADER = null;
        try { XHR_GET_HEADER = XMLHttpRequest.prototype.getResponseHeader; } catch (e) { XHR_GET_HEADER = null; }
        var xhrNet = new WeakMap();
        var XHR_NET_GET = Function.prototype.call.bind(WeakMap.prototype.get);
        var XHR_NET_SET = Function.prototype.call.bind(WeakMap.prototype.set);
        function xhrState(xhr) {
            var st = XHR_NET_GET(xhrNet, xhr);
            if (!st) {
                st = { net: null, entry: null, hooked: false, mock: null, mime: null };
                XHR_NET_SET(xhrNet, xhr, st);
            }
            return st;
        }
        function xhrEvent(type, loaded, total) {
            if (typeof ProgressEvent !== 'function') return new Event(type);
            return new ProgressEvent(type, { lengthComputable: total > 0, loaded: loaded || 0, total: total || 0 });
        }
        function xhrFinish(entry, status) {
            entry.status = status;
            entry.duration_ms = Date.now() - entry.timestamp;
        }
        function xhrLog(entry) {
            appendCapped(networkLog, entry, CAP_NETWORK);
        }
        function xhrUnlog(entry) {
            for (var i = networkLog.length - 1; i >= 0; i--) {
                if (networkLog[i] === entry) { removeAt(networkLog, i); return; }
            }
        }
        function xhrHook(xhr, st) {
            if (st.hooked) return;
            st.hooked = true;
            var on = function(type, fn) { REFLECT_APPLY(ADD_LISTENER, xhr, [type, fn]); };
            on('load', function() {
                var e = st.entry;
                if (!e || e.status !== 'pending') return;
                e.status_text = xhr.statusText;
                // An XHR to an IPC URL is in the IPC log too: record Tauri's command outcome,
                // as the fetch path does — without it a "command not found" (HTTP 200 +
                // `Tauri-Response: error`) logged as ok and "verified" a ghost (red team F7).
                if (XHR_GET_HEADER && isIpcUrl(e.url)) {
                    try { e.ipc_response = REFLECT_APPLY(XHR_GET_HEADER, xhr, ['Tauri-Response']); } catch (err) {}
                }
                xhrFinish(e, xhr.status);
            });
            on('error', function() {
                var e = st.entry;
                if (e && e.status === 'pending') xhrFinish(e, 'error');
            });
            on('abort', function() {
                var e = st.entry;
                if (e && e.status === 'pending') xhrFinish(e, 'aborted');
            });
            on('timeout', function() {
                var e = st.entry;
                if (e && e.status === 'pending') xhrFinish(e, 'timeout');
            });
            // Backstop: whatever ended the request, it must not stay 'pending' forever (that
            // would wedge wait_for network_idle / ipc_idle).
            on('loadend', function() {
                var e = st.entry;
                if (e && e.status === 'pending') xhrFinish(e, 'error');
            });
        }

        // ── `fulfill` ──
        //
        // A mocked request never reaches the native XHR, which stays OPENED. Everything the page
        // reads about the response comes from the mock instead: the prototype's accessors and
        // header methods ask this per-XHR state first, so nothing shows on the instance, and an XHR
        // with no mock (or a `this` that is not an XHR) still gets the native behaviour and errors.
        var XHR_PROTO = XMLHttpRequest.prototype;
        var TEXT_ENCODER = null;
        try { TEXT_ENCODER = new window.TextEncoder(); } catch (e) { TEXT_ENCODER = null; }
        var TEXT_ENCODE = TEXT_ENCODER ? window.TextEncoder.prototype.encode : null;
        var BLOB_CTOR = typeof window.Blob === 'function' ? window.Blob : null;
        var DOM_PARSER = typeof window.DOMParser === 'function' ? window.DOMParser : null;
        var DOM_PARSE = DOM_PARSER ? window.DOMParser.prototype.parseFromString : null;
        function xhrMockOf(xhr) {
            var st = XHR_NET_GET(xhrNet, xhr);
            return st ? st.mock : null;
        }
        function mockBytes(m) {
            if (m.bytes === undefined) {
                m.bytes = TEXT_ENCODER ? REFLECT_APPLY(TEXT_ENCODE, TEXT_ENCODER, [m.body]) : null;
            }
            return m.bytes;
        }
        function mockByteLength(m) {
            var bytes = mockBytes(m);
            return bytes ? bytes.length : m.body.length;
        }
        // Parsed as a real response is: HTML only when the page asked for a document, a document
        // that is not well-formed or of another type is null.
        function mockDocument(m, xhr, htmlAllowed) {
            if (m.document !== undefined) return m.document;
            var st = XHR_NET_GET(xhrNet, xhr);
            var mime = st.mime || m.headers['content-type'] || '';
            var semi = REFLECT_APPLY(STR_INDEXOF, mime, [';']);
            mime = REFLECT_APPLY(STR_TO_LOWER, semi < 0 ? mime : REFLECT_APPLY(STR_SUBSTRING, mime, [0, semi]), []).trim();
            var isHtml = mime === 'text/html';
            var isXml = mime === 'text/xml' || mime === 'application/xml'
                || (mime.length > 4 && REFLECT_APPLY(STR_SUBSTRING, mime, [mime.length - 4]) === '+xml');
            var doc = null;
            if (DOM_PARSER && ((isHtml && htmlAllowed) || isXml)) {
                try {
                    doc = REFLECT_APPLY(DOM_PARSE, new DOM_PARSER(), [m.body, isHtml ? 'text/html' : 'application/xml']);
                    if (isXml && doc.getElementsByTagName('parsererror').length) doc = null;
                } catch (e) { doc = null; }
            }
            if (isHtml && !htmlAllowed) return null; // not cached: `responseType` can still change
            m.document = doc;
            return doc;
        }
        function mockResponse(m, xhr, type) {
            if (m.state !== 4 || m.failed) return null;
            if (m.value !== undefined) return m.value;
            var value = null;
            if (type === 'json') {
                try { value = PRISTINE_PARSE(m.body); } catch (e) { value = null; }
            } else if (type === 'arraybuffer') {
                var bytes = mockBytes(m);
                value = bytes ? REFLECT_APPLY(ARRAY_BUFFER_SLICE, bytes.buffer, [bytes.byteOffset, bytes.byteOffset + bytes.length]) : null;
            } else if (type === 'blob') {
                value = BLOB_CTOR ? new BLOB_CTOR(mockBytes(m) ? [mockBytes(m)] : [m.body], { type: m.headers['content-type'] || '' }) : null;
            } else if (type === 'document') {
                return mockDocument(m, xhr, true);
            }
            m.value = value;
            return value;
        }
        function hasHeaders(m) { return m.state >= 2 && !m.failed; }
        function xhrAccessor(name, read) {
            var d = Object.getOwnPropertyDescriptor(XHR_PROTO, name);
            if (!d || !d.get) return;
            var native = d.get;
            Object.defineProperty(XHR_PROTO, name, {
                configurable: d.configurable, enumerable: d.enumerable, set: d.set,
                get: function() {
                    var m = xhrMockOf(this);
                    return m ? read(m, this, native) : REFLECT_APPLY(native, this, []);
                },
            });
        }
        var XHR_TYPE_GET = (Object.getOwnPropertyDescriptor(XHR_PROTO, 'responseType') || {}).get;
        function xhrType(xhr) { return XHR_TYPE_GET ? REFLECT_APPLY(XHR_TYPE_GET, xhr, []) : ''; }
        xhrAccessor('readyState', function(m) { return m.state; });
        xhrAccessor('status', function(m) { return hasHeaders(m) ? m.status : 0; });
        xhrAccessor('statusText', function(m) { return hasHeaders(m) ? m.status_text : ''; });
        xhrAccessor('responseURL', function(m) { return hasHeaders(m) ? m.url : ''; });
        xhrAccessor('responseText', function(m, xhr, native) {
            var type = xhrType(xhr);
            // The native getter throws this engine's own InvalidStateError for another type.
            if (type !== '' && type !== 'text') return REFLECT_APPLY(native, xhr, []);
            return (m.state >= 3 && !m.failed) ? m.body : '';
        });
        xhrAccessor('response', function(m, xhr) {
            var type = xhrType(xhr);
            if (type === '' || type === 'text') return (m.state >= 3 && !m.failed) ? m.body : '';
            return mockResponse(m, xhr, type);
        });
        xhrAccessor('responseXML', function(m, xhr, native) {
            var type = xhrType(xhr);
            if (type !== '' && type !== 'document') return REFLECT_APPLY(native, xhr, []);
            if (m.state !== 4 || m.failed) return null;
            return mockDocument(m, xhr, type === 'document');
        });
        var origGetAll = XHR_PROTO.getAllResponseHeaders;
        var origOverrideMime = XHR_PROTO.overrideMimeType;
        XHR_PROTO.getResponseHeader = function(name) {
            var m = xhrMockOf(this);
            if (!m || arguments.length < 1) return REFLECT_APPLY(XHR_GET_HEADER, this, arguments);
            if (!hasHeaders(m)) return null;
            var key = REFLECT_APPLY(STR_TO_LOWER, toStringExact(name), []);
            // Never readable from a response, as for a real one.
            if (key === 'set-cookie' || key === 'set-cookie2') return null;
            return m.headers[key] === undefined ? null : '' + m.headers[key];
        };
        XHR_PROTO.getAllResponseHeaders = function() {
            var m = xhrMockOf(this);
            if (!m) return REFLECT_APPLY(origGetAll, this, arguments);
            if (!hasHeaders(m)) return '';
            var names = [];
            for (var k in m.headers) { if (k !== 'set-cookie' && k !== 'set-cookie2') names[names.length] = k; }
            names.sort();
            var out = '';
            for (var i = 0; i < names.length; i++) out += names[i] + ': ' + m.headers[names[i]] + '\r\n';
            return out;
        };
        XHR_PROTO.overrideMimeType = function(mime) {
            var ret = REFLECT_APPLY(origOverrideMime, this, arguments);
            var st = xhrState(this);
            st.mime = toStringExact(mime);
            return ret;
        };
        function xhrFire(xhr, event) {
            try { REFLECT_APPLY(DISPATCH_EVENT, xhr, [event]); } catch (e) {}
        }
        // The native object's url, as `responseURL` gives it: absolute, no fragment.
        function xhrResponseUrl(url) {
            var abs = url;
            try {
                var base = BASE_URI_GET ? REFLECT_APPLY(BASE_URI_GET, document, []) : window.location.href;
                abs = REFLECT_APPLY(URL_HREF_GET, new URL_CTOR(url, base), []);
            } catch (e) {}
            var hash = REFLECT_APPLY(STR_INDEXOF, abs, ['#']);
            return hash < 0 ? abs : REFLECT_APPLY(STR_SUBSTRING, abs, [0, hash]);
        }
        function xhrMockEnd(entry, m, url) {
            entry.status_text = m.status_text;
            if (isIpcUrl(url)) entry.ipc_response = m.headers['tauri-response'] === 'ok' ? 'ok' : 'error';
            xhrFinish(entry, m.status);
        }
        // Answer a matching request with the rule's mock: `send` does not reach the network. The
        // transitions come in one later task, stopping as soon as a handler moved the XHR on.
        function xhrFulfill(xhr, st, entry, route, net, body) {
            var m = {
                state: 1, failed: false, timer: null,
                status: route.status, status_text: route.status_text,
                headers: mockHeaders(route),
                body: mockBody(route) || '',
                url: xhrResponseUrl(net.url),
            };
            st.mock = m;
            st.entry = entry;
            entry.mocked = true;
            if (route.delay_ms > 0) entry.route_delay_ms = route.delay_ms;
            xhrLog(entry);
            var size = mockByteLength(m);
            if (!net.async) {
                // Nothing can wait inside a synchronous send().
                xhrMockEnd(entry, m, net.url);
                m.state = 4;
                xhrFire(xhr, new Event('readystatechange'));
                if (st.mock === m && m.state === 4) xhrFire(xhr, xhrEvent('load', size, size));
                if (st.mock === m && m.state === 4) xhrFire(xhr, xhrEvent('loadend', size, size));
                return;
            }
            var hasBody = body !== null && body !== undefined && net.method !== 'GET' && net.method !== 'HEAD';
            var sent = typeof body === 'string' ? body.length : 0;
            xhrFire(xhr, xhrEvent('loadstart', 0, 0));
            if (hasBody) xhrFire(xhr.upload, xhrEvent('loadstart', 0, sent));
            if (st.mock !== m || m.state !== 1) return;
            function moved(state) { return st.mock !== m || m.state !== state; }
            function answer() {
                m.timer = null;
                if (moved(1)) return;
                if (hasBody) {
                    xhrFire(xhr.upload, xhrEvent('progress', sent, sent));
                    xhrFire(xhr.upload, xhrEvent('load', sent, sent));
                    xhrFire(xhr.upload, xhrEvent('loadend', sent, sent));
                    if (moved(1)) return;
                }
                m.state = 2;
                xhrFire(xhr, new Event('readystatechange'));
                if (moved(2)) return;
                // With no body there is no LOADING state and no progress.
                if (size > 0) {
                    m.state = 3;
                    xhrFire(xhr, new Event('readystatechange'));
                    if (moved(3)) return;
                    xhrFire(xhr, xhrEvent('progress', size, size));
                    if (moved(3)) return;
                }
                xhrMockEnd(entry, m, net.url);
                m.state = 4;
                xhrFire(xhr, new Event('readystatechange'));
                if (moved(4)) return;
                xhrFire(xhr, xhrEvent('load', size, size));
                if (moved(4)) return;
                xhrFire(xhr, xhrEvent('loadend', size, size));
            }
            // A `timeout` the mock's delay runs past ends it as it would end a slow request.
            var limit = xhr.timeout;
            if (limit > 0 && route.delay_ms >= limit) {
                m.timer = SET_TIMEOUT(function() {
                    m.timer = null;
                    if (moved(1)) return;
                    m.failed = true;
                    m.state = 4;
                    xhrFinish(entry, 'timeout');
                    xhrFire(xhr, new Event('readystatechange'));
                    if (moved(4)) return;
                    xhrFire(xhr, xhrEvent('timeout', 0, 0));
                    if (moved(4)) return;
                    xhrFire(xhr, xhrEvent('loadend', 0, 0));
                }, limit);
            } else {
                m.timer = SET_TIMEOUT(answer, route.delay_ms);
            }
        }
        // As on a real request: in flight it ends with `readystatechange`, `abort`, `loadend`; once
        // it has ended there are no events. Either way it reads as UNSENT afterwards.
        function xhrMockAbort(xhr, st, m) {
            if (m.timer !== null) { CLEAR_TIMEOUT(m.timer); m.timer = null; }
            if (m.state < 4 && m.state > 0) {
                m.failed = true;
                m.state = 4;
                if (st.entry && st.entry.status === 'pending') xhrFinish(st.entry, 'aborted');
                xhrFire(xhr, new Event('readystatechange'));
                if (st.mock === m && m.state === 4) xhrFire(xhr, xhrEvent('abort', 0, 0));
                if (st.mock === m && m.state === 4) xhrFire(xhr, xhrEvent('loadend', 0, 0));
            }
            if (st.mock === m) { m.failed = true; m.state = 0; }
        }

        XMLHttpRequest.prototype.open = function(method, url) {
            // No early delete: an open() that throws leaves the previous request intact (spec),
            // and a successful one replaces this entry below.
            if (arguments.length < 2) return REFLECT_APPLY(origOpen, this, arguments);
            // Convert ONCE, exactly as open() does (a URL object or anything with toString), and
            // hand open() the converted strings: what is logged is what is requested. A
            // conversion that throws is left for open() itself to throw.
            var m, u;
            try { m = toStringExact(method); u = toStringExact(url); }
            catch (e) { return REFLECT_APPLY(origOpen, this, arguments); }
            var args = [m, u];
            for (var i = 2; i < arguments.length; i++) args[i] = arguments[i];
            var ret = REFLECT_APPLY(origOpen, this, args);
            var st = xhrState(this);
            // A new request: nothing of a mocked one is left. The native object never left OPENED,
            // so it did not announce the state change a page sees.
            var announce = false;
            if (st.mock) {
                if (st.mock.timer !== null) CLEAR_TIMEOUT(st.mock.timer);
                announce = st.mock.state !== 1;
                st.mock = null;
            }
            // open() on a request in flight terminates it without firing any event: close its
            // entry here, or it would stay 'pending' forever.
            if (st.entry && st.entry.status === 'pending') xhrFinish(st.entry, 'aborted');
            // ... and a request still in its route delay is gone with it.
            if (st.delay_timer !== null && st.delay_timer !== undefined) {
                CLEAR_TIMEOUT(st.delay_timer);
                st.delay_timer = null;
            }
            st.entry = null;
            // An explicit third argument is converted even when undefined (false), as open() does.
            st.net = { method: m, url: u, async: arguments.length < 3 || !!arguments[2] };
            if (announce) xhrFire(this, new Event('readystatechange'));
            return ret;
        };
        // abort() during a route delay: the real send has not happened, so the native abort is a
        // no-op and the request would still go out after the delay.
        XMLHttpRequest.prototype.abort = function() {
            var st = XHR_NET_GET(xhrNet, this);
            if (st && st.mock) return xhrMockAbort(this, st, st.mock);
            if (st && st.delay_timer !== null && st.delay_timer !== undefined) {
                CLEAR_TIMEOUT(st.delay_timer);
                st.delay_timer = null;
                if (st.entry && st.entry.status === 'pending') xhrFinish(st.entry, 'aborted');
                try {
                    REFLECT_APPLY(DISPATCH_EVENT, this, [xhrEvent('abort')]);
                    REFLECT_APPLY(DISPATCH_EVENT, this, [xhrEvent('loadend')]);
                } catch (e) {}
                return undefined;
            }
            return REFLECT_APPLY(origAbort, this, arguments);
        };
        XMLHttpRequest.prototype.send = function() {
            var st = XHR_NET_GET(xhrNet, this);
            // The native object is still OPENED and would really send: this one has been sent.
            if (st && st.mock) throw new DOMException('The object\'s state must be OPENED.', 'InvalidStateError');
            var net = st && st.net;
            if (!net || isVictauriInternalUrl(net.url)) {
                return REFLECT_APPLY(origSend, this, arguments);
            }
            // Each send() is its own request with its own log entry.
            var entry = nullProto({
                id: ++networkCounter,
                method: net.method.toUpperCase(),
                url: loggedUrl(net.url),
                timestamp: Date.now(),
                status: 'pending',
                duration_ms: null,
            });
            xhrHook(this, st);
            var self = this;

            // Phase 1 routing for XHR: block, delay, and fulfill (see `xhrFulfill`).
            var xroute = matchRoute(net.url, net.method);
            if (xroute) {
                recordRouteMatch(xroute, net.url, net.method);
                entry.route_rule = xroute.id;
                if (xroute.action === 'fulfill') {
                    xhrFulfill(this, st, entry, xroute, net, arguments[0]);
                    return undefined;
                }
                if (xroute.action === 'block') {
                    st.entry = entry;
                    entry.blocked = true;
                    xhrFinish(entry, 'blocked');
                    xhrLog(entry);
                    // End it like a network failure — `readystatechange`, `error`, `loadend` —
                    // unless the app has moved this XHR on to another request meanwhile. (`readyState`
                    // stays 1: only a request that was really sent can reach 4.)
                    SET_TIMEOUT(function() {
                        if (st.entry !== entry) return;
                        try {
                            REFLECT_APPLY(DISPATCH_EVENT, self, [new Event('readystatechange')]);
                            REFLECT_APPLY(DISPATCH_EVENT, self, [xhrEvent('error')]);
                            REFLECT_APPLY(DISPATCH_EVENT, self, [xhrEvent('loadend')]);
                        } catch (e) {}
                    }, 0);
                    return; // do not send
                }
                if (xroute.action === 'delay' && xroute.delay_ms > 0) {
                    st.entry = entry;
                    xhrLog(entry);
                    var dArgs = arguments;
                    var delayTimer = SET_TIMEOUT(function() {
                        if (st.delay_timer === delayTimer) st.delay_timer = null;
                        // Re-opened meanwhile: this request no longer exists.
                        if (st.entry !== entry || entry.status !== 'pending') return;
                        try { REFLECT_APPLY(origSend, self, dArgs); }
                        catch (e) { entry.error = String(e); xhrFinish(entry, 'error'); }
                    }, xroute.delay_ms);
                    st.delay_timer = delayTimer;
                    return;
                }
            }
            // The entry is current BEFORE the real send(): a synchronous XHR fires its events
            // inside send(). A send() that throws (not opened, already sent) starts no request:
            // its entry is dropped and the request still in flight stays current.
            var previous = st.entry;
            st.entry = entry;
            xhrLog(entry);
            try {
                return REFLECT_APPLY(origSend, this, arguments);
            } catch (e) {
                if (st.entry === entry) st.entry = previous;
                xhrUnlog(entry);
                throw e;
            }
        };
    })();

    // ── postMessage IPC observation (see "IPC transport" above) ──────────────
    //
    // `JSON.stringify` is wrapped so the message Tauri serializes for `window.ipc.postMessage`
    // can be observed. The wrapper returns exactly what the native call returns (or throws
    // what it throws) and inspects nothing unless a FUNCTION replacer was passed — Tauri's
    // processIpcMessage always passes one — so ordinary page calls only pay one typeof check.
    // The bridge's own serialization uses NATIVE_STRINGIFY (captured above), never this.
    (function interceptPostMessageIpc() {
        var J = window.JSON;
        if (!J || typeof NATIVE_STRINGIFY !== 'function') return;
        // A method, not a `function` expression: like the native one it cannot be called with
        // `new` and has no `prototype` (red team F5).
        var stringify = ({
            stringify(value, replacer, space) {
                'use strict';
                var out = REFLECT_APPLY(NATIVE_STRINGIFY, J, arguments);
                if (typeof replacer === 'function' && typeof out === 'string'
                    && value !== null && typeof value === 'object') {
                    try { observePostMessageIpc(value, out); } catch (e) {}
                }
                return out;
            }
        }).stringify;
        try { J.stringify = stringify; STRINGIFY_HOOK = stringify; } catch (e) {}
    })();

    // ── Navigation Tracking ──────────────────────────────────────────────────

    (function trackNavigation() {
        // URLs can be huge (a state in the hash): stored cut, as the network log stores them.
        function logNav(type, oldUrl) {
            var e = { url: loggedUrl(window.location.href), timestamp: Date.now(), type: type };
            if (oldUrl !== undefined) e.old_url = loggedUrl(oldUrl);
            appendCapped(navigationLog, e, CAP_NAVIGATION);
        }
        logNav('initial');

        var origPushState = history.pushState;
        var origReplaceState = history.replaceState;
        history.pushState = function() {
            var result = origPushState.apply(this, arguments);
            logNav('pushState');
            return result;
        };
        history.replaceState = function() {
            var result = origReplaceState.apply(this, arguments);
            logNav('replaceState');
            return result;
        };
        window.addEventListener('popstate', function() { logNav('popstate'); });
        window.addEventListener('hashchange', function(e) { logNav('hashchange', e.oldURL); });
    })();

    // ── Dialog Capture ───────────────────────────────────────────────────────

    // Default fail-CLOSED (audit #32): merely loading the bridge must not silently
    // auto-approve "are you sure?" gates. confirm() -> false, prompt() -> null until
    // an explicit set_dialog_response opts into accepting.
    var dialogAutoResponses = { alert: { action: 'accept' }, confirm: { action: 'dismiss' }, prompt: { action: 'dismiss', text: '' } };

    // ── Resource Cleanup ────────────────────────────────────────────────────

    // A page entering the back/forward cache (`persisted`) is frozen, not unloaded: it can be
    // restored as-is and the init script does NOT run again. Tearing capture down here left a
    // restored page with console and DOM-mutation capture permanently off. So a persisted
    // pagehide keeps everything, and `pageshow` re-installs whatever a teardown removed.
    //
    // Both listeners act ONLY on the browser's own (trusted) events. Page script can dispatch a
    // synthetic `pagehide` at will (`isTrusted` is false and unforgeable); honouring one let a
    // page wipe every captured log and switch console + mutation capture off for good (R4-JS1).
    var captureTornDown = false;
    window.addEventListener('pageshow', function(e) {
        if (!e || e.isTrusted !== true || !e.persisted || !captureTornDown) return;
        captureTornDown = false;
        hookConsole('log');
        hookConsole('warn');
        hookConsole('error');
        hookConsole('info');
        hookConsole('debug');
        if (!__mutationObserver) startMutationObserver();
    });

    window.addEventListener('pagehide', function(e) {
        if (!e || e.isTrusted !== true || e.persisted) return;
        captureTornDown = true;
        if (__mutationObserver) { __mutationObserver.disconnect(); __mutationObserver = null; }
        if (mutationBatchTimer) { clearTimeout(mutationBatchTimer); mutationBatchTimer = null; }
        console.log = originalConsole.log;
        console.warn = originalConsole.warn;
        console.error = originalConsole.error;
        console.info = originalConsole.info;
        console.debug = originalConsole.debug;
        consoleLogs.length = 0;
        mutationLog.length = 0;
        networkLog.length = 0;
        navigationLog.length = 0;
        dialogLog.length = 0;
        interactionLog.length = 0;
        REFLECT_APPLY(MAP_CLEAR, refMap, []);
        REFLECT_APPLY(MAP_CLEAR, weakRefMap, []);
        nodeIds = new WeakMap();
    });

    (function captureDialogs() {
        // `alert(0)` shows "0" and `alert()` nothing. Bounded: a log entry is kept for good.
        function dialogText(msg) { return msg === undefined ? '' : truncText(safeArgString(msg), MAX_CONSOLE_MSG); }
        function logDialog(e) { appendCapped(dialogLog, e, CAP_DIALOG); }
        window.alert = function(msg) {
            logDialog({ type: 'alert', message: dialogText(msg), timestamp: Date.now() });
        };
        window.confirm = function(msg) {
            var resp = dialogAutoResponses.confirm;
            var result = resp.action === 'accept';
            logDialog({ type: 'confirm', message: dialogText(msg), timestamp: Date.now(), result: result });
            return result;
        };
        window.prompt = function(msg, defaultValue) {
            var resp = dialogAutoResponses.prompt;
            // An explicit '' is an answer; only an absent one falls back to the default.
            var answer = (resp.text !== undefined && resp.text !== null) ? resp.text
                : (defaultValue !== undefined && defaultValue !== null ? defaultValue : '');
            var result = resp.action === 'accept' ? answer : null;
            logDialog({ type: 'prompt', message: dialogText(msg), timestamp: Date.now(), result: result });
            return result;
        };
    })();

    // Signal to the Rust backend that the JS bridge is fully initialized, identifying this page
    // load by its nonce. A page restored from the back/forward cache re-runs no init script, so
    // it re-announces itself: an eval armed in the page it replaced must not wait out its timeout.
    function signalReady() {
        evalCallback('__victauri_bridge_ready__', PAGE_NONCE);
    }
    window.addEventListener('pageshow', function(e) { if (e && e.isTrusted === true && e.persisted) signalReady(); });
    signalReady();
})();
"#;
