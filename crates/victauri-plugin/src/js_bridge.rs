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

/// The body of the init script (after capacity variable declarations).
/// Uses CAP_* variables for all log limits.
const INIT_SCRIPT_BODY: &str = r#"
    var refMap = new Map();
    var refCounter = 0;
    var weakRefMap = new Map();

    function resolveRef(refId) {
        var direct = refMap.get(refId);
        if (direct) {
            if (direct.isConnected) return direct;
            refMap.delete(refId);
            return null;
        }
        var weak = weakRefMap.get(refId);
        if (weak) {
            var el = weak.deref();
            if (el && el.isConnected) return el;
            weakRefMap.delete(refId);
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
        var ref_id = nodeIds.get(node);
        if (ref_id === undefined) {
            ref_id = 'e' + (refCounter++);
            nodeIds.set(node, ref_id);
        }
        if (refMap.has(ref_id)) refMap.delete(ref_id); // re-insert as newest
        if (refMap.size >= REF_MAP_LIMIT) {
            // Drop the oldest STRONG reference only. With WeakRef support the ref
            // stays resolvable through weakRefMap while its node is alive, so a
            // snapshot larger than the limit never invalidates its own early refs.
            var oldest = refMap.keys().next().value;
            refMap.delete(oldest);
            if (!HAS_WEAKREF) weakRefMap.delete(oldest);
        }
        refMap.set(ref_id, node);
        if (HAS_WEAKREF) {
            var existing = weakRefMap.get(ref_id);
            if (!existing || existing.deref() !== node) {
                weakRefMap.set(ref_id, new WeakRef(node));
            }
        }
        if (buildingRefs) buildingRefs.add(ref_id);
        return ref_id;
    }

    function getStaleRefs() {
        var stale = [];
        weakRefMap.forEach(function(weak, refId) {
            var el = weak.deref();
            if (!el || !el.isConnected) {
                stale.push(refId);
                weakRefMap.delete(refId);
                refMap.delete(refId);
            }
        });
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
    var IPC_PREFIXES = ['http://ipc.localhost/', 'ipc://localhost/'];
    function ipcCommandPath(url) {
        if (typeof url !== 'string') {
            try { url = String(url); } catch (e) { return null; }
        }
        for (var pi = 0; pi < IPC_PREFIXES.length; pi++) {
            if (url.indexOf(IPC_PREFIXES[pi]) === 0) return url.substring(IPC_PREFIXES[pi].length);
        }
        return null;
    }
    function isIpcUrl(url) { return ipcCommandPath(url) !== null; }
    // Victauri's own IPC (plugin:victauri|*). Decided from the parsed IPC command path — NOT a
    // substring anywhere in the URL, which let any page request containing
    // "plugin%3Avictauri%7C" in a query string escape route rules and network logging.
    function isVictauriInternalUrl(url) {
        var p = ipcCommandPath(url);
        if (p === null) return false;
        try { p = decodeURIComponent(p); } catch (e) {}
        return p.indexOf('plugin:victauri|') === 0;
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

    // Convert a glob to a RegExp. `*` (any run of characters) is the only wildcard; every other
    // character matches itself — including `?`, which used to slip through unescaped as a regex
    // quantifier, so `*/api/search?q=*` matched nothing (R5-JS4).
    function globToRegExp(glob) {
        var re = glob.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*');
        return new RegExp('^' + re + '$');
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
    function routeUrlForms(url) {
        var forms = [url];
        if (!URL_CTOR || !URL_HREF_GET) return forms;
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
            if (r.match_type === 'regex') return new RegExp(r.pattern).test(url);
            if (r.match_type === 'glob') return globToRegExp(r.pattern).test(url);
            return url.indexOf(r.pattern) !== -1; // substring (default)
        } catch (e) { return false; }
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

    function recordRouteMatch(rule, url, method) {
        rule.triggered = (rule.triggered || 0) + 1;
        routeMatchLog.push({
            rule_id: rule.id, action: rule.action, url: url,
            method: (method || 'GET').toUpperCase(), timestamp: Date.now(),
            trigger_count: rule.triggered,
        });
        if (routeMatchLog.length > CAP_ROUTE_MATCHES) routeMatchLog.shift();
    }

    function checkActionable(el) {
        if (!el || !el.isConnected) return { error: 'element is detached from DOM', hint: 'RETRY_LATER' };
        if (el.disabled) return { error: 'element is disabled (disabled attribute)', hint: 'RETRY_LATER' };
        if (el.getAttribute && el.getAttribute('aria-disabled') === 'true') return { error: 'element is disabled (aria-disabled)', hint: 'RETRY_LATER' };
        // Use the element's OWN document/window so the viewport and occlusion
        // (elementFromPoint) checks are correct for elements inside same-origin
        // iframes — getBoundingClientRect() is relative to the element's own
        // frame viewport, not the top document.
        var doc = el.ownerDocument || document;
        var win = doc.defaultView || window;
        var cs = win.getComputedStyle(el);
        if (cs.display === 'none') return { error: 'element is not visible (display: none)', hint: 'RETRY_LATER' };
        if (cs.visibility === 'hidden') return { error: 'element is not visible (visibility: hidden)', hint: 'RETRY_LATER' };
        if (parseFloat(cs.opacity) < 0.01) return { error: 'element is not visible (opacity: ' + cs.opacity + ')', hint: 'RETRY_LATER' };
        var rect = el.getBoundingClientRect();
        if (rect.width === 0 && rect.height === 0) return { error: 'element has zero size', hint: 'RETRY_LATER' };
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
        if (topEl && topEl !== el && !composedContains(el, topEl) && !composedContains(topEl, el)) {
            var tag = topEl.tagName ? topEl.tagName.toLowerCase() : 'unknown';
            var info = tag;
            if (topEl.id) info += '#' + topEl.id;
            else if (topEl.className && typeof topEl.className === 'string') {
                var cls = topEl.className.trim().split(/\s+/)[0];
                if (cls) info += '.' + cls;
            }
            return { error: 'element is covered by ' + info + ' at (' + Math.round(cx) + ',' + Math.round(cy) + ')', hint: 'RETRY_LATER' };
        }
        return null;
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
                    if (Date.now() >= deadline) { resolve({ ok: false, error: 'ref not found: ' + refId, hint: 'CHECK_INPUT' }); return; }
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
    var SET_TIMEOUT = window.setTimeout.bind(window);
    var REFLECT_APPLY = Reflect.apply;
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
    function PRISTINE_STRINGIFY(v) { return NATIVE_STRINGIFY(v, neutralReplacer); }

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
    // The per-eval state used to live on a page-visible global (`window.__VIC_EVAL__`), so page
    // script could enumerate pending eval ids and forge their results via the callback command,
    // or suppress every result. It now lives in this closure, reachable only through the frozen,
    // non-configurable `__VICTAURI__` methods below, which never reveal an id. Serialization uses
    // `JSON.stringify` captured here, at init, before any page script can replace it.
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
        var type = payload && payload.__victauri_type;
        if (type === 'value') {
            // The code RAN; a result JSON cannot carry (circular, BigInt, a function, a
            // throwing toJSON) is reported as exactly that, never as a JavaScript error.
            var value = payload.__victauri_ok;
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
        if (payload && hasOwn(payload, '__victauri_err')) {
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
    var ASSIGN = Object.assign;
    function copyEntries(arr) {
        var out = new Array(arr.length);
        for (var i = 0; i < arr.length; i++) {
            var e = arr[i];
            if (e && typeof e === 'object') {
                var c = ASSIGN({}, e);
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
    function ipcCallCommand(n) {
        var raw = ipcCommandPath(n.url);
        if (raw === null || raw.indexOf(IPC_LOG_VICTAURI_PREFIX) === 0) return null;
        try { return decodeURIComponent(raw); } catch (e) { return raw; }
    }
    // The newest app IPC call in the network log (the live entry — never hand it out).
    function newestIpcCall() {
        for (var i = networkLog.length - 1; i >= 0; i--) {
            if (ipcCallCommand(networkLog[i]) !== null) return networkLog[i];
        }
        return null;
    }
    // The `getIpcLog` view of one IPC network entry: a fresh object whose nested values are
    // deep copies, so the page cannot rewrite a logged call through it. Without `bodies`, the
    // request args and result are left out and the error text is capped.
    var MAX_IPC_ERROR_TEXT = 4096;
    function ipcLogEntry(n, bodies) {
        // Classify by COMMAND outcome, not just HTTP status. Tauri returns
        // HTTP 200 for a failed command (incl. "command not found") and signals
        // the real result via the `Tauri-Response` header captured as ipc_response.
        // Precedence: pending > transport error (HTTP >= 400 / 'error') > command
        // error (ipc_response 'error') > ok.
        var st;
        if (n.status === 'pending') { st = 'pending'; }
        else if (n.status !== 200 && n.status !== 'ok') { st = 'error'; }
        else if (n.ipc_response === 'error') { st = 'error'; }
        else { st = 'ok'; }
        var errText = null;
        if (st === 'error') {
            if (n.status !== 200 && n.status !== 'ok' && n.status !== 'pending') {
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
        var e = {
            id: n.id,
            command: ipcCallCommand(n),
            timestamp: n.timestamp,
            status: st,
            duration_ms: n.duration_ms,
            error: errText,
        };
        if (bodies) {
            e.args = cloneJson(n.request_args) || {};
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
        consoleLogs.forEach(function(l) {
            emit({ type: 'console', level: l.level, message: l.message, timestamp: l.timestamp }, l.timestamp, l);
        });
        mutationLog.forEach(function(m) {
            emit({ type: 'dom_mutation', count: m.count, timestamp: m.timestamp }, m.timestamp, m);
        });
        networkLog.forEach(function(n) {
            var isPending = n.status === 'pending';
            if (isPending && skipPending) return;
            var key = isPending ? n.timestamp : n.timestamp + (n.duration_ms || 0);
            var mocked = n.mocked === true || n.blocked === true;
            var raw = ipcCommandPath(n.url);
            if (raw === null) {
                // Plain network traffic. IPC requests are emitted once, as `ipc` events —
                // not a second time as `network`.
                var nev = { type: 'network', method: n.method, url: n.url, status: n.status, duration_ms: n.duration_ms, timestamp: n.timestamp, seq_ts: key };
                if (mocked) nev.mocked = true;
                emit(nev, key, n);
                return;
            }
            if (raw.indexOf(IPC_VICTAURI_PREFIX) === 0) return;
            var cmd; try { cmd = decodeURIComponent(raw); } catch(e) { cmd = raw; }
            // Same classification as getIpcLog: HTTP 200 with a
            // `Tauri-Response: error` header is a failed command.
            var st;
            if (isPending) st = 'pending';
            else if (n.status !== 200 && n.status !== 'ok') st = 'error';
            else if (n.ipc_response === 'error') st = 'error';
            else st = 'ok';
            var iev = { type: 'ipc', command: cmd, status: st, duration_ms: n.duration_ms, arg_size_bytes: n.arg_size_bytes || 0, timestamp: n.timestamp, seq_ts: key };
            if (mocked) iev.mocked = true;
            emit(iev, key, n);
        });
        navigationLog.forEach(function(n) {
            emit({ type: 'navigation', url: n.url, nav_type: n.type, timestamp: n.timestamp }, n.timestamp, n);
        });
        interactionLog.forEach(function(i) {
            emit({ type: 'dom_interaction', action: i.action, selector: i.selector, value: i.value, timestamp: i.timestamp }, i.timestamp, i);
        });
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
            var previousRefs = new Set(refMap.keys());
            refMap.clear();
            var fmt = format || 'compact';
            var tree;
            var currentRefs = new Set();
            buildingRefs = currentRefs;
            try {
                if (fmt === 'json') {
                    tree = walkDom(document.body);
                } else {
                    tree = walkDomCompact(document.body, 0);
                }
            } finally {
                buildingRefs = null;
            }
            var stale = [];
            var staleSet = new Set();
            function markStale(rid) {
                if (!staleSet.has(rid)) { staleSet.add(rid); stale.push(rid); }
            }
            previousRefs.forEach(function(refId) {
                if (!currentRefs.has(refId)) {
                    var weak = weakRefMap.get(refId);
                    if (weak) {
                        var el = weak.deref();
                        if (!el || !el.isConnected) {
                            markStale(refId);
                            weakRefMap.delete(refId);
                        }
                    } else {
                        markStale(refId);
                    }
                }
            });
            // Prune dead or detached entries so weakRefMap stays bounded by the
            // live DOM (refs of the snapshot just built are never pruned).
            weakRefMap.forEach(function(weak, rid) {
                if (currentRefs.has(rid)) return;
                var el = weak.deref();
                if (!el || !el.isConnected) {
                    weakRefMap.delete(rid);
                    refMap.delete(rid);
                    markStale(rid);
                }
            });
            return { tree: tree, stale_refs: stale, format: fmt };
        },

        getRef: function(refId) {
            return resolveRef(refId);
        },

        getStaleRefs: function() {
            return getStaleRefs();
        },

        findElements: function(query) {
            var results = [];
            var maxResults = query.max_results || 10;

            if (query.css) {
                // Validate against a detached fragment, not document.body: while the page is
                // reloading `body` is null, and the resulting TypeError used to be reported as
                // "invalid CSS selector" for a perfectly valid selector.
                try { document.createDocumentFragment().querySelector(query.css); } catch(e) {
                    return { error: 'invalid CSS selector: ' + query.css + ' — ' + e.message };
                }
            }
            if (!document.body) {
                return { error: 'page not ready: the document has no body yet (it may be loading or reloading) — retry shortly' };
            }

            function matches(el) {
                if (query.text) {
                    var txt = (el.textContent || '').trim();
                    if (query.exact) {
                        if (txt !== query.text) return false;
                    } else {
                        if (txt.toLowerCase().indexOf(query.text.toLowerCase()) === -1) return false;
                    }
                }
                if (query.role) {
                    var role = el.getAttribute('role') || inferRole(el);
                    if (role !== query.role) return false;
                }
                if (query.test_id) {
                    if (el.getAttribute('data-testid') !== query.test_id) return false;
                }
                if (query.css) {
                    if (!el.matches(query.css)) return false;
                }
                if (query.name) {
                    var name = el.getAttribute('aria-label')
                        || el.getAttribute('title')
                        || el.getAttribute('placeholder') || '';
                    if (name.toLowerCase().indexOf(query.name.toLowerCase()) === -1) return false;
                }
                if (query.tag) {
                    if (el.tagName.toLowerCase() !== query.tag.toLowerCase()) return false;
                }
                if (query.placeholder) {
                    if ((el.getAttribute('placeholder') || '').toLowerCase().indexOf(query.placeholder.toLowerCase()) === -1) return false;
                }
                if (query.alt) {
                    if ((el.getAttribute('alt') || '').toLowerCase().indexOf(query.alt.toLowerCase()) === -1) return false;
                }
                if (query.title_attr) {
                    if ((el.getAttribute('title') || '').toLowerCase().indexOf(query.title_attr.toLowerCase()) === -1) return false;
                }
                if (query.enabled === true && el.disabled) return false;
                if (query.enabled === false && !el.disabled) return false;
                return true;
            }

            function buildResult(node, style) {
                // registerRef reuses the node's existing id (if any).
                var ref_id = registerRef(node);
                var role = node.getAttribute('role') || inferRole(node);
                var rect = node.getBoundingClientRect();
                var vis = true;
                if (style) {
                    vis = style.display !== 'none' && style.visibility !== 'hidden';
                }
                return {
                    ref_id: ref_id,
                    tag: node.tagName.toLowerCase(),
                    role: role,
                    name: node.getAttribute('aria-label') || node.getAttribute('title') || null,
                    text: truncText((node.textContent || '').trim(), 100),
                    bounds: { x: Math.round(rect.x), y: Math.round(rect.y), width: Math.round(rect.width), height: Math.round(rect.height) },
                    visible: vis,
                    enabled: !node.disabled,
                    value: isPasswordInput(node) ? '[REDACTED]' : safeValue(node)
                };
            }

            if (query.label) {
                var labels = document.querySelectorAll('label');
                for (var li = 0; li < labels.length && results.length < maxResults; li++) {
                    var lbl = labels[li];
                    if ((lbl.textContent || '').toLowerCase().indexOf(query.label.toLowerCase()) === -1) continue;
                    var target = null;
                    var forAttr = lbl.getAttribute('for');
                    if (forAttr) {
                        target = document.getElementById(forAttr);
                    }
                    if (!target) {
                        target = lbl.querySelector('input, textarea, select');
                    }
                    if (target) {
                        var ts = window.getComputedStyle(target);
                        results.push(buildResult(target, ts));
                    }
                }
                return results;
            }

            function search(node) {
                if (results.length >= maxResults) return;
                if (!node || node.nodeType !== 1) return;
                var style = window.getComputedStyle(node);
                if (style.display === 'none' || style.visibility === 'hidden') return;

                if (matches(node)) {
                    results.push(buildResult(node, style));
                }

                for (var c = 0; c < node.children.length; c++) {
                    search(node.children[c]);
                }
                if (node.shadowRoot) {
                    for (var s = 0; s < node.shadowRoot.children.length; s++) {
                        search(node.shadowRoot.children[s]);
                    }
                }
                // Same-origin iframe traversal.
                if (node.tagName === 'IFRAME' || node.tagName === 'FRAME') {
                    try {
                        var idoc = node.contentDocument;
                        if (idoc && idoc.body) search(idoc.body);
                    } catch (e) { /* cross-origin: skip */ }
                }
            }

            search(document.body);
            return results;
        },

        // ── Interactions ─────────────────────────────────────────────────────

        click: function(refId, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                el.click();
                return { ok: true };
            });
        },

        doubleClick: function(refId, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
                return { ok: true };
            });
        },

        hover: function(refId, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                el.dispatchEvent(new MouseEvent('mouseenter', { bubbles: true }));
                el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
                return { ok: true };
            });
        },

        fill: function(refId, value, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                if (!el.matches('input, textarea, [contenteditable="true"]')) {
                    return { ok: false, error: 'element is not fillable (not input, textarea, or contenteditable): ' + (el.tagName || '').toLowerCase(), hint: 'CHECK_INPUT' };
                }
                if (el.tagName !== 'INPUT' && el.tagName !== 'TEXTAREA') {
                    // contenteditable: there is no `value` — the HTMLInputElement
                    // value setter would throw "Illegal invocation" here.
                    el.textContent = value;
                    el.dispatchEvent(new Event('input', { bubbles: true }));
                    return { ok: true };
                }
                var proto = el instanceof HTMLTextAreaElement
                    ? HTMLTextAreaElement.prototype
                    : HTMLInputElement.prototype;
                var desc = Object.getOwnPropertyDescriptor(proto, 'value');
                if (desc && desc.set) {
                    desc.set.call(el, value);
                } else {
                    el.value = value;
                }
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return { ok: true };
            });
        },

        type: function(refId, text, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                el.focus();
                var proto = el instanceof HTMLTextAreaElement
                    ? HTMLTextAreaElement.prototype
                    : HTMLInputElement.prototype;
                var desc = Object.getOwnPropertyDescriptor(proto, 'value');
                for (var i = 0; i < text.length; i++) {
                    var ch = text[i];
                    el.dispatchEvent(new KeyboardEvent('keydown', { key: ch, bubbles: true }));
                    el.dispatchEvent(new KeyboardEvent('keypress', { key: ch, bubbles: true }));
                    var current = el.value || '';
                    if (desc && desc.set) {
                        desc.set.call(el, current + ch);
                    } else {
                        el.value = current + ch;
                    }
                    el.dispatchEvent(new InputEvent('input', { bubbles: true, data: ch, inputType: 'insertText' }));
                    el.dispatchEvent(new KeyboardEvent('keyup', { key: ch, bubbles: true }));
                }
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return { ok: true };
            });
        },

        pressKey: function(key) {
            var target = document.activeElement || document.body;
            var parts = key.split('+');
            if (parts.length === 1 || (parts.length === 2 && parts[0] === '' && parts[1] === '')) {
                var k = parts.length === 1 ? key : '+';
                target.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true }));
                target.dispatchEvent(new KeyboardEvent('keyup', { key: k, bubbles: true }));
                return { ok: true };
            }
            var finalKey = parts.pop();
            var mods = { ctrlKey: false, shiftKey: false, altKey: false, metaKey: false };
            for (var m = 0; m < parts.length; m++) {
                var mod = parts[m];
                if (mod === 'Control' || mod === 'Ctrl') mods.ctrlKey = true;
                else if (mod === 'Shift') mods.shiftKey = true;
                else if (mod === 'Alt') mods.altKey = true;
                else if (mod === 'Meta' || mod === 'Command' || mod === 'Cmd') mods.metaKey = true;
            }
            var modKeys = [];
            if (mods.ctrlKey) modKeys.push('Control');
            if (mods.shiftKey) modKeys.push('Shift');
            if (mods.altKey) modKeys.push('Alt');
            if (mods.metaKey) modKeys.push('Meta');
            for (var i = 0; i < modKeys.length; i++) {
                target.dispatchEvent(new KeyboardEvent('keydown', { key: modKeys[i], bubbles: true, ctrlKey: mods.ctrlKey, shiftKey: mods.shiftKey, altKey: mods.altKey, metaKey: mods.metaKey }));
            }
            target.dispatchEvent(new KeyboardEvent('keydown', { key: finalKey, bubbles: true, ctrlKey: mods.ctrlKey, shiftKey: mods.shiftKey, altKey: mods.altKey, metaKey: mods.metaKey }));
            target.dispatchEvent(new KeyboardEvent('keyup', { key: finalKey, bubbles: true, ctrlKey: mods.ctrlKey, shiftKey: mods.shiftKey, altKey: mods.altKey, metaKey: mods.metaKey }));
            for (var j = modKeys.length - 1; j >= 0; j--) {
                target.dispatchEvent(new KeyboardEvent('keyup', { key: modKeys[j], bubbles: true, ctrlKey: mods.ctrlKey, shiftKey: mods.shiftKey, altKey: mods.altKey, metaKey: mods.metaKey }));
            }
            return { ok: true };
        },

        selectOption: function(refId, values, timeoutMs) {
            return withAutoWait(refId, timeoutMs, function(el) {
                if (el.tagName !== 'SELECT') {
                    return { ok: false, error: 'element is not a <select>', hint: 'CHECK_INPUT' };
                }
                var valSet = new Set(values);
                for (var i = 0; i < el.options.length; i++) {
                    el.options[i].selected = valSet.has(el.options[i].value);
                }
                el.dispatchEvent(new Event('change', { bubbles: true }));
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
            var bodies = !(opts && opts.bodies === false);
            var idSet = null;
            if (opts && opts.ids) {
                idSet = OBJ_CREATE(null);
                for (var j = 0; j < opts.ids.length; j++) idSet['' + opts.ids[j]] = true;
            }
            var picked = [];
            for (var i = 0; i < networkLog.length; i++) {
                var n = networkLog[i];
                if (idSet && !idSet['' + n.id]) continue;
                if (ipcCallCommand(n) === null) continue;
                picked.push(n);
            }
            var start = (typeof limit === 'number' && limit > 0 && picked.length > limit) ? picked.length - limit : 0;
            var entries = [];
            for (var k = start; k < picked.length; k++) entries.push(ipcLogEntry(picked[k], bodies));
            return entries;
        },

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
            return copyEntries(since ? consoleLogs.filter(function(l) { return l.timestamp >= since; }) : consoleLogs);
        },

        // ── Mutations ────────────────────────────────────────────────────────

        getMutationLog: function(since) {
            return copyEntries(since ? mutationLog.filter(function(m) { return m.timestamp >= since; }) : mutationLog);
        },

        // ── Network ──────────────────────────────────────────────────────────

        // Filtered and limited BEFORE copying (R5-JS1). `opts.bodies === false` leaves out
        // the captured IPC request args / response bodies (e.g. to count entries cheaply).
        getNetworkLog: function(filter, limit, opts) {
            var log = networkLog;
            if (filter) {
                log = log.filter(function(e) { return e.url.indexOf(filter) !== -1; });
            }
            if (typeof limit === 'number' && limit > 0) log = log.slice(-limit);
            if (opts && opts.bodies === false) {
                var out = new Array(log.length);
                for (var i = 0; i < log.length; i++) {
                    var c = ASSIGN({}, log[i]);
                    delete c.request_args;
                    delete c.response_body;
                    out[i] = c;
                }
                return copyEntries(out);
            }
            return copyEntries(log);
        },

        // ── Network routing (interception / mock / block / delay) ──────────────
        // Add a route rule. `rule` is an object: { pattern, match_type, method,
        // action ('block'|'fulfill'|'delay'), status, status_text, headers,
        // body, content_type, delay_ms, times }. Returns the assigned id.
        addRoute: function(rule) {
            // The captured parse, not the page's: page script could rewrite every rule the agent adds.
            if (typeof rule === 'string') { try { rule = PRISTINE_PARSE(rule); } catch (e) { return { ok: false, error: 'invalid rule JSON' }; } }
            if (!rule || !rule.pattern) return { ok: false, error: 'route rule requires a pattern' };
            if ((rule.action || 'fulfill') === 'fulfill' && typeof rule.status === 'number'
                && (rule.status !== Math.floor(rule.status) || rule.status < 200 || rule.status > 599)) {
                return { ok: false, error: 'fulfill status must be an integer in 200-599 (a Response cannot carry ' + rule.status + ')' };
            }
            var r = {
                id: ++routeCounter,
                pattern: String(rule.pattern),
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
            routeRules.push(r);
            return { ok: true, id: r.id, rule: r };
        },

        getRouteRules: function() { return copyEntries(routeRules); },

        getRouteMatches: function(limit) {
            return copyEntries(limit ? routeMatchLog.slice(-limit) : routeMatchLog);
        },

        // Agent-only operations (clear logs / route rules, dialog auto-responses, animation
        // scrub / sweep recorder) are NOT on
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
                var seq = drainSeqs.get(entry);
                if (seq === undefined) {
                    seq = ++drainSeq;
                    drainSeqs.set(entry, seq);
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
            return new Promise(function(resolve) {
                var timeout = opts.timeout_ms || 10000;
                var poll = opts.poll_ms || 200;
                var start = Date.now();

                function check() {
                    var elapsed = Date.now() - start;
                    if (elapsed >= timeout) {
                        resolve({ ok: false, error: 'timeout after ' + timeout + 'ms', elapsed_ms: elapsed });
                        return;
                    }

                    function getFullText(root) {
                        // ShadowRoot has no innerText — fall back to textContent.
                        var text = (typeof root.innerText === 'string' ? root.innerText : root.textContent) || '';
                        var els = root.querySelectorAll('*');
                        for (var j = 0; j < els.length; j++) {
                            if (els[j].shadowRoot) text += ' ' + getFullText(els[j].shadowRoot);
                        }
                        return text;
                    }
                    var met = false;
                    if (opts.condition === 'text' && opts.value) {
                        met = getFullText(document.body).indexOf(opts.value) !== -1;
                    } else if (opts.condition === 'text_gone' && opts.value) {
                        met = getFullText(document.body).indexOf(opts.value) === -1;
                    } else if (opts.condition === 'selector' && opts.value) {
                        met = !!document.querySelector(opts.value);
                    } else if (opts.condition === 'selector_gone' && opts.value) {
                        met = !document.querySelector(opts.value);
                    } else if (opts.condition === 'url' && opts.value) {
                        met = window.location.href.indexOf(opts.value) !== -1;
                    } else if (opts.condition === 'ipc_idle') {
                        met = networkLog.filter(function(n) { return isIpcUrl(n.url); }).every(function(n) { return n.status !== 'pending'; });
                    } else if (opts.condition === 'network_idle') {
                        met = networkLog.every(function(n) { return n.status !== 'pending'; });
                    }

                    if (met) {
                        resolve({ ok: true, elapsed_ms: Date.now() - start });
                    } else {
                        setTimeout(check, poll);
                    }
                }
                check();
            });
        },
        // ── CSS / Style Introspection ────────────────────────────────────────

        getStyles: function(refId, properties) {
            var el = resolveRef(refId);
            if (!el) return { error: 'ref not found: ' + refId };
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
                if (!el) { results.push({ ref_id: refIds[i], error: 'ref not found' }); continue; }
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
            if (!el) return { error: 'ref not found: ' + refId };
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
                var hasLabel = false;
                if (inp.id) {
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
                if (!text && !ariaLabel && !link.title) {
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
                max_depth: (function() { var d = 0; var walk = function(el, depth) { if (depth > d) d = depth; for (var i = 0; i < el.children.length && i < 5; i++) walk(el.children[i], depth + 1); }; walk(document.body, 0); return d; })(),
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

    // See `_agent`: reachable only with the per-process agent key. Built in a STRICT function so
    // every op — and every callback an op creates — is strict: a page hook on a built-in an op
    // calls then cannot reach the op through `.caller` (and call it without the key), nor the
    // injected script beyond it whose source carries the key (R4-JS2).
    var AGENT_OPS = (function() {
    'use strict';
    var AGENT_OPS = OBJ_CREATE(null);
    AGENT_OPS.clearIpcLog = function() {
        for (var i = networkLog.length - 1; i >= 0; i--) {
            if (isIpcUrl(networkLog[i].url)) networkLog.splice(i, 1);
        }
        return { ok: true };
    };
    AGENT_OPS.clearNetworkLog = function() { networkLog.length = 0; return { ok: true }; };
    AGENT_OPS.clearConsoleLogs = function() { consoleLogs.length = 0; return { ok: true }; };
    AGENT_OPS.clearMutationLog = function() { mutationLog.length = 0; return { ok: true }; };
    AGENT_OPS.clearDialogLog = function() { dialogLog.length = 0; return { ok: true }; };
    AGENT_OPS.clearRoute = function(id) {
        var before = routeRules.length;
        routeRules = routeRules.filter(function(r) { return r.id !== id; });
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
            anims.forEach(function(a) { try { a.pause(); } catch (e) {} });
            scrubState = { el: el, anims: anims, ends: ends, duration: duration };
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
                try { S.anims[i].currentTime = Math.max(0, Math.min(t, S.ends[i])); } catch (e) {}
            }
            return Promise.all(S.anims.map(function(a) { return a.ready.catch(function(){}); }))
                .then(function() {
                    return new Promise(function(res) {
                        requestAnimationFrame(function() { requestAnimationFrame(res); });
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
            S.anims.forEach(function(a) { try { if (resume) a.play(); } catch (e) {} });
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
                if (sweepState !== R) return;
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
            if (R.stopped) {
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
                longTasks.push({ duration: entries[i].duration, startTime: entries[i].startTime });
                if (longTasks.length > CAP_LONG_TASKS) longTasks.shift();
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

    function walkDom(node) {
        if (!node || node.nodeType !== 1) return null;

        var style = window.getComputedStyle(node);
        var visible = style.display !== 'none'
            && style.visibility !== 'hidden'
            && style.opacity !== '0';

        if (!visible) return null;

        var ref_id = registerRef(node);

        var rect = node.getBoundingClientRect();
        var role = node.getAttribute('role') || inferRole(node);
        var name = node.getAttribute('aria-label')
            || node.getAttribute('title')
            || node.getAttribute('placeholder')
            || (node.tagName === 'BUTTON' ? truncText(node.textContent.trim(), 80) : null)
            || (node.tagName === 'A' ? truncText(node.textContent.trim(), 80) : null);

        var element = {
            ref_id: ref_id,
            tag: node.tagName.toLowerCase(),
            role: role,
            name: name,
            text: getDirectText(node),
            value: isPasswordInput(node) ? '[REDACTED]' : safeValue(node),
            enabled: !node.disabled,
            visible: true,
            focusable: node.tabIndex >= 0 || ['INPUT','BUTTON','SELECT','TEXTAREA','A'].indexOf(node.tagName) !== -1,
            bounds: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
            children: [],
            attributes: {}
        };

        var interestingAttrs = ['data-testid', 'id', 'type', 'href', 'src', 'checked', 'selected'];
        for (var a = 0; a < interestingAttrs.length; a++) {
            if (node.hasAttribute(interestingAttrs[a])) {
                element.attributes[interestingAttrs[a]] = node.getAttribute(interestingAttrs[a]);
            }
        }

        for (var c = 0; c < node.children.length; c++) {
            var childEl = walkDom(node.children[c]);
            if (childEl) element.children.push(childEl);
        }

        if (node.shadowRoot) {
            for (var s = 0; s < node.shadowRoot.children.length; s++) {
                var shadowChild = walkDom(node.shadowRoot.children[s]);
                if (shadowChild) element.children.push(shadowChild);
            }
        }

        // Same-origin iframe traversal: descend into accessible frame documents.
        // Cross-origin frames throw on contentDocument access — mark and skip.
        if (node.tagName === 'IFRAME' || node.tagName === 'FRAME') {
            try {
                var idoc = node.contentDocument;
                if (idoc && idoc.body) {
                    var frameChild = walkDom(idoc.body);
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

        return element;
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
        return node.tagName === 'INPUT' && (node.getAttribute('type') || '').toLowerCase() === 'password';
    }

    // A page-derived string for the compact tree, always as a quoted JSON string. U+2028 /
    // U+2029 / U+0085 are escaped too: JSON leaves them raw, and a reader may treat them as a
    // line break (and so as the start of a forged `[eN] …` line).
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

    function walkDomCompact(node, depth) {
        if (!node || node.nodeType !== 1) return '';

        var style = window.getComputedStyle(node);
        var visible = style.display !== 'none'
            && style.visibility !== 'hidden'
            && style.opacity !== '0';

        if (!visible) return '';

        var ref_id = registerRef(node);
        var indent = '';
        for (var d = 0; d < depth; d++) indent += '  ';

        var role = node.getAttribute('role') || inferRole(node);
        var name = node.getAttribute('aria-label')
            || node.getAttribute('title')
            || node.getAttribute('placeholder')
            || '';
        var text = getDirectText(node) || '';
        var tag = node.tagName.toLowerCase();

        // Grammar, one line per element (every page-derived field is a bare plain token or a
        // JSON string, so markup cannot forge a line or a field):
        //   [eN] <head> [role=<json>] [<json name/text>] [[disabled]] [value=<json>]
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
        // RSS title, a chat message) forged whole `[eN] button "…"` lines in this tree and could
        // steer an agent's click onto a different element.
        if (name) {
            line += ' ' + compactStr(truncText(name, 60));
        } else if (text && text.length <= 60) {
            line += ' ' + compactStr(text);
        } else if (text) {
            line += ' ' + compactStr(truncText(text, 57) + '...');
        }

        if (node.disabled) line += ' [disabled]';
        var value = safeValue(node);
        if (value !== null) {
            line += ' value=' + compactStr(isPasswordInput(node) ? '[REDACTED]' : truncText(value, 40));
        }

        var testId = node.getAttribute('data-testid');
        if (testId) line += ' @' + compactAttr(testId);

        var type = node.getAttribute('type');
        if (type && tag === 'input') line += ' type=' + compactAttr(type);

        var href = node.getAttribute('href');
        if (href && tag === 'a') line += ' href=' + compactAttr(truncText(href, 60));

        var result = line + '\n';

        for (var c = 0; c < node.children.length; c++) {
            result += walkDomCompact(node.children[c], depth + 1);
        }

        if (node.shadowRoot) {
            for (var s = 0; s < node.shadowRoot.children.length; s++) {
                result += walkDomCompact(node.shadowRoot.children[s], depth + 1);
            }
        }

        // Same-origin iframe traversal (see walkDom for rationale).
        if (node.tagName === 'IFRAME' || node.tagName === 'FRAME') {
            try {
                var idoc = node.contentDocument;
                if (idoc && idoc.body) {
                    result += indent + '  ⤷ iframe content:\n';
                    result += walkDomCompact(idoc.body, depth + 2);
                } else {
                    result += indent + '  ⤷ [cross-origin iframe]\n';
                }
            } catch (e) {
                result += indent + '  ⤷ [cross-origin iframe]\n';
            }
        }

        return result;
    }

    function inferRole(node) {
        var tag = node.tagName;
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
            var type = node.getAttribute('type');
            if (type === 'checkbox') return 'checkbox';
            if (type === 'radio') return 'radio';
            if (type === 'range') return 'slider';
            if (type === 'submit' || type === 'button') return 'button';
        }
        return roles[tag] || null;
    }

    function getDirectText(node) {
        var text = '';
        for (var i = 0; i < node.childNodes.length; i++) {
            if (node.childNodes[i].nodeType === 3) text += node.childNodes[i].textContent;
        }
        text = text.trim();
        return text.length > 0 ? truncText(text, 200) : null;
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
            try {
                var msg = '';
                var skippedArgs = 0;
                for (var i = 0; i < arguments.length; i++) {
                    if (msg.length > MAX_CONSOLE_MSG) { skippedArgs = arguments.length - i; break; }
                    if (i) msg += ' ';
                    msg += safeArgString(arguments[i]);
                }
                consoleLogs.push({ level: level, message: capConsoleMessage(msg, skippedArgs), timestamp: Date.now() });
                if (consoleLogs.length > CAP_CONSOLE) consoleLogs.shift();
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
            consoleLogs.push({ level: 'error', message: capConsoleMessage('[uncaught] ' + msg, 0), timestamp: Date.now() });
            if (consoleLogs.length > CAP_CONSOLE) consoleLogs.shift();
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
            consoleLogs.push({ level: 'error', message: capConsoleMessage('[unhandled rejection] ' + msg, 0), timestamp: Date.now() });
            if (consoleLogs.length > CAP_CONSOLE) consoleLogs.shift();
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
        interactionLog.push({
            type: 'dom_interaction',
            action: action,
            selector: bestSelector(el),
            value: value || null,
            timestamp: Date.now()
        });
        if (interactionLog.length > CAP_INTERACTION) interactionLog.shift();
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
                    mutationLog.push({ count: mutationBatchCount, timestamp: Date.now() });
                    if (mutationLog.length > CAP_MUTATION) mutationLog.shift();
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
                var entry = { id: id, method: method.toUpperCase(), url: url, timestamp: Date.now(), status: 'pending', duration_ms: null };

                if (isIpc && !isVictauriInternal) {
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
                                    entry.request_args = JSON.parse(reqBody);
                                }
                            }
                        } catch(e) {}
                    }
                }

                if (!isVictauriInternal) {
                    networkLog.push(entry);
                    if (networkLog.length > CAP_NETWORK) networkLog.shift();
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
                    if (route.action === 'block') {
                        entry.status = 'blocked';
                        entry.blocked = true;
                        entry.duration_ms = Date.now() - entry.timestamp;
                        if (isIpc) flushIpcWaiters();
                        return Promise.reject(new TypeError('victauri: request blocked by route #' + route.id + ' (' + url + ')'));
                    }
                    if (route.action === 'fulfill') {
                        var makeResp = function() {
                            var status = route.status;
                            if (typeof status !== 'number' || status !== Math.floor(status) || status < 200 || status > 599) {
                                throw new RangeError('victauri: route #' + route.id + ' fulfill status must be an integer in 200-599, got ' + status);
                            }
                            // The Response constructor throws if a null-body status carries a body.
                            var nullBody = status === 204 || status === 205 || status === 304;
                            var bodyStr = nullBody ? null
                                : ((typeof route.body === 'string') ? route.body : JSON.stringify(route.body));
                            var hdrs = { 'content-type': route.content_type };
                            for (var k in route.headers) { if (Object.prototype.hasOwnProperty.call(route.headers, k)) hdrs[k] = route.headers[k]; }
                            var resp = new Response(bodyStr, { status: status, statusText: route.status_text, headers: hdrs });
                            entry.status = status;
                            entry.status_text = route.status_text;
                            entry.mocked = true;
                            entry.duration_ms = Date.now() - entry.timestamp;
                            if (isIpc) {
                                if (bodyStr === null) entry.response_body = null;
                                else if (bodyStr.length > MAX_IPC_BODY) entry.response_body = bodyOmitted(bodyStr.length, null);
                                else { try { entry.response_body = JSON.parse(bodyStr); } catch (e) { entry.response_body = bodyStr; } }
                                flushIpcWaiters();
                            }
                            return resp;
                        };
                        // Build the Response inside the promise chain so a construction
                        // error becomes a rejection (as a real fetch failure would), never
                        // a synchronous throw out of fetch().
                        var waitP = route.delay_ms > 0
                            ? new Promise(function(res) { setTimeout(res, route.delay_ms); })
                            : Promise.resolve();
                        return waitP.then(makeResp).catch(function(err) {
                            entry.status = 'error';
                            entry.error = String(err);
                            entry.duration_ms = Date.now() - entry.timestamp;
                            if (isIpc) flushIpcWaiters();
                            throw err;
                        });
                    }
                    if (route.action === 'delay' && route.delay_ms > 0) {
                        return new Promise(function(resolve, reject) {
                            setTimeout(function() { doRealFetch().then(resolve, reject); }, route.delay_ms);
                        });
                    }
                }
                return doRealFetch();

                function doRealFetch() {
                    return REFLECT_APPLY(origFetch, self, fetchArgs(fetchInput)).then(function(response) {
                        entry.status = response.status;
                        entry.status_text = response.statusText;
                        entry.duration_ms = Date.now() - entry.timestamp;

                        if (isIpc) {
                            // Capture Tauri's command-outcome signal. The HTTP status is 200
                            // for BOTH a successful command AND a failed/"not found" one — the
                            // real Ok/Err result is carried in the `Tauri-Response` header
                            // ('ok' | 'error'). Without this, every IPC call logs as "ok",
                            // which blinds ghost detection (an unregistered command looks like
                            // a verified handler). 'ok' | 'error' | null (older Tauri / no hdr).
                            try { entry.ipc_response = response.headers.get('Tauri-Response'); } catch (e) {}
                            var ct = null, clen = -1;
                            try {
                                ct = response.headers.get('Content-Type');
                                var cl = response.headers.get('Content-Length');
                                if (cl !== null && cl !== '' && isFinite(Number(cl))) clen = Number(cl);
                            } catch (e) {}
                            if (window.__VICTAURI__._captureIpcBodies !== false && !isTextualContentType(ct)) {
                                // Binary (e.g. raw tauri::ipc::Response bytes): never read it.
                                entry.response_body = bodyOmitted(clen, ct);
                                flushIpcWaiters();
                            } else if (window.__VICTAURI__._captureIpcBodies !== false && clen > MAX_IPC_BODY) {
                                entry.response_body = bodyOmitted(clen, null);
                                flushIpcWaiters();
                            } else if (window.__VICTAURI__._captureIpcBodies !== false) {
                                var cloned = response.clone();
                                cloned.text().then(function(text) {
                                    if (text.length > MAX_IPC_BODY) {
                                        entry.response_body = bodyOmitted(text.length, null);
                                        return;
                                    }
                                    try { entry.response_body = JSON.parse(text); } catch(e) { entry.response_body = text; }
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
        var ADD_LISTENER = EventTarget.prototype.addEventListener;
        var DISPATCH_EVENT = EventTarget.prototype.dispatchEvent;
        // Per-XHR state in a closure WeakMap — not an expando on the XHR, which page script
        // could overwrite to forge the entry `send()` logs (R4-JS3): the {method, url} `open()`
        // really received, and the log entry of the request currently in flight. The WeakMap
        // methods are captured before page script runs.
        //
        // The bridge's listeners are attached ONCE per XHR and always update the CURRENT entry
        // (R5B-XHR1). Attaching them on every send() piled them up on a reused XHR, each closing
        // over its own send's entry, so every later request rewrote the earlier entries.
        var xhrNet = new WeakMap();
        var XHR_NET_GET = Function.prototype.call.bind(WeakMap.prototype.get);
        var XHR_NET_SET = Function.prototype.call.bind(WeakMap.prototype.set);
        function xhrState(xhr) {
            var st = XHR_NET_GET(xhrNet, xhr);
            if (!st) {
                st = { net: null, entry: null, hooked: false };
                XHR_NET_SET(xhrNet, xhr, st);
            }
            return st;
        }
        function xhrEvent(type) {
            return typeof ProgressEvent === 'function' ? new ProgressEvent(type) : new Event(type);
        }
        function xhrFinish(entry, status) {
            entry.status = status;
            entry.duration_ms = Date.now() - entry.timestamp;
        }
        function xhrLog(entry) {
            networkLog.push(entry);
            if (networkLog.length > CAP_NETWORK) networkLog.shift();
        }
        function xhrUnlog(entry) {
            for (var i = networkLog.length - 1; i >= 0; i--) {
                if (networkLog[i] === entry) { networkLog.splice(i, 1); return; }
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
            // open() on a request in flight terminates it without firing any event: close its
            // entry here, or it would stay 'pending' forever.
            if (st.entry && st.entry.status === 'pending') xhrFinish(st.entry, 'aborted');
            st.entry = null;
            st.net = { method: m, url: u };
            return ret;
        };
        XMLHttpRequest.prototype.send = function() {
            var st = XHR_NET_GET(xhrNet, this);
            var net = st && st.net;
            if (!net || isVictauriInternalUrl(net.url)) {
                return REFLECT_APPLY(origSend, this, arguments);
            }
            // Each send() is its own request with its own log entry.
            var entry = {
                id: ++networkCounter,
                method: net.method.toUpperCase(),
                url: net.url,
                timestamp: Date.now(),
                status: 'pending',
                duration_ms: null,
            };
            xhrHook(this, st);
            var self = this;

            // Phase 1 routing for XHR: block + delay are supported here.
            // `fulfill` (synthetic response) is fetch-only — faking the full
            // XHR response surface is unreliable; document as a limitation.
            var xroute = matchRoute(net.url, net.method);
            if (xroute) {
                recordRouteMatch(xroute, net.url, net.method);
                if (xroute.action === 'block') {
                    st.entry = entry;
                    entry.blocked = true;
                    xhrFinish(entry, 'blocked');
                    xhrLog(entry);
                    // End it like a network failure — `error`, then `loadend` — unless the app
                    // has moved this XHR on to another request meanwhile.
                    SET_TIMEOUT(function() {
                        if (st.entry !== entry) return;
                        try {
                            REFLECT_APPLY(DISPATCH_EVENT, self, [xhrEvent('error')]);
                            REFLECT_APPLY(DISPATCH_EVENT, self, [xhrEvent('loadend')]);
                        } catch (e) {}
                    }, 0);
                    return; // do not send
                }
                if ((xroute.action === 'delay' || xroute.action === 'fulfill') && xroute.delay_ms > 0) {
                    st.entry = entry;
                    xhrLog(entry);
                    var dArgs = arguments;
                    SET_TIMEOUT(function() {
                        // Re-opened meanwhile: this request no longer exists.
                        if (st.entry !== entry || entry.status !== 'pending') return;
                        try { REFLECT_APPLY(origSend, self, dArgs); }
                        catch (e) { entry.error = String(e); xhrFinish(entry, 'error'); }
                    }, xroute.delay_ms);
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

    // ── Navigation Tracking ──────────────────────────────────────────────────

    (function trackNavigation() {
        navigationLog.push({ url: window.location.href, timestamp: Date.now(), type: 'initial' });

        var origPushState = history.pushState;
        var origReplaceState = history.replaceState;
        history.pushState = function() {
            var result = origPushState.apply(this, arguments);
            navigationLog.push({ url: window.location.href, timestamp: Date.now(), type: 'pushState' });
            if (navigationLog.length > CAP_NAVIGATION) navigationLog.shift();
            return result;
        };
        history.replaceState = function() {
            var result = origReplaceState.apply(this, arguments);
            navigationLog.push({ url: window.location.href, timestamp: Date.now(), type: 'replaceState' });
            if (navigationLog.length > CAP_NAVIGATION) navigationLog.shift();
            return result;
        };
        window.addEventListener('popstate', function() {
            navigationLog.push({ url: window.location.href, timestamp: Date.now(), type: 'popstate' });
            if (navigationLog.length > CAP_NAVIGATION) navigationLog.shift();
        });
        window.addEventListener('hashchange', function(e) {
            navigationLog.push({ url: window.location.href, timestamp: Date.now(), type: 'hashchange', old_url: e.oldURL });
            if (navigationLog.length > CAP_NAVIGATION) navigationLog.shift();
        });
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
        refMap.clear();
        weakRefMap.clear();
        nodeIds = new WeakMap();
        refCounter = 0;
    });

    (function captureDialogs() {
        window.alert = function(msg) {
            dialogLog.push({ type: 'alert', message: String(msg || ''), timestamp: Date.now() });
            if (dialogLog.length > CAP_DIALOG) dialogLog.shift();
        };
        window.confirm = function(msg) {
            var resp = dialogAutoResponses.confirm;
            var result = resp.action === 'accept';
            dialogLog.push({ type: 'confirm', message: String(msg || ''), timestamp: Date.now(), result: result });
            if (dialogLog.length > CAP_DIALOG) dialogLog.shift();
            return result;
        };
        window.prompt = function(msg, defaultValue) {
            var resp = dialogAutoResponses.prompt;
            var result = resp.action === 'accept' ? (resp.text || defaultValue || '') : null;
            dialogLog.push({ type: 'prompt', message: String(msg || ''), timestamp: Date.now(), result: result });
            if (dialogLog.length > CAP_DIALOG) dialogLog.shift();
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
