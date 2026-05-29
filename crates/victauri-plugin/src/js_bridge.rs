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
    ) + INIT_SCRIPT_BODY
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

    function registerRef(node) {
        var ref_id = 'e' + (refCounter++);
        if (refMap.size >= REF_MAP_LIMIT) {
            var oldest = refMap.keys().next().value;
            refMap.delete(oldest);
            weakRefMap.delete(oldest);
        }
        refMap.set(ref_id, node);
        if (typeof WeakRef !== 'undefined') {
            weakRefMap.set(ref_id, new WeakRef(node));
        }
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
    var navigationLog = [];
    var dialogLog = [];
    var interactionLog = [];
    var CAP_INTERACTION = 500;
    var ipcWaiters = [];
    var longTasks = [];
    var listenerCount = 0;

    function checkActionable(el) {
        if (!el || !el.isConnected) return { error: 'element is detached from DOM', hint: 'RETRY_LATER' };
        if (el.disabled) return { error: 'element is disabled (disabled attribute)', hint: 'RETRY_LATER' };
        if (el.getAttribute && el.getAttribute('aria-disabled') === 'true') return { error: 'element is disabled (aria-disabled)', hint: 'RETRY_LATER' };
        var cs = window.getComputedStyle(el);
        if (cs.display === 'none') return { error: 'element is not visible (display: none)', hint: 'RETRY_LATER' };
        if (cs.visibility === 'hidden') return { error: 'element is not visible (visibility: hidden)', hint: 'RETRY_LATER' };
        if (parseFloat(cs.opacity) < 0.01) return { error: 'element is not visible (opacity: ' + cs.opacity + ')', hint: 'RETRY_LATER' };
        var rect = el.getBoundingClientRect();
        if (rect.width === 0 && rect.height === 0) return { error: 'element has zero size', hint: 'RETRY_LATER' };
        if (cs.pointerEvents === 'none') return { error: 'element has pointer-events: none', hint: 'RETRY_LATER' };
        var vw = window.innerWidth || document.documentElement.clientWidth;
        var vh = window.innerHeight || document.documentElement.clientHeight;
        if (rect.bottom < 0 || rect.top > vh || rect.right < 0 || rect.left > vw) {
            el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
            rect = el.getBoundingClientRect();
            if (rect.bottom < 0 || rect.top > vh || rect.right < 0 || rect.left > vw) {
                return { error: 'element is outside viewport after scroll attempt', hint: 'CHECK_INPUT' };
            }
        }
        var cx = rect.left + rect.width / 2;
        var cy = rect.top + rect.height / 2;
        var topEl = document.elementFromPoint(cx, cy);
        if (topEl && topEl !== el && !el.contains(topEl) && !topEl.contains(el)) {
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

    // ── Public API ───────────────────────────────────────────────────────────

    window.__VICTAURI__ = {
        version: '0.6.0',
        _captureIpcBodies: true,

        // ── DOM ──────────────────────────────────────────────────────────────

        snapshot: function(format) {
            var previousRefs = new Set(refMap.keys());
            refMap.clear();
            var fmt = format || 'compact';
            var tree;
            if (fmt === 'json') {
                tree = walkDom(document.body);
            } else {
                tree = walkDomCompact(document.body, 0);
            }
            var currentRefs = new Set(refMap.keys());
            var stale = [];
            previousRefs.forEach(function(refId) {
                if (!currentRefs.has(refId)) {
                    var weak = weakRefMap.get(refId);
                    if (weak) {
                        var el = weak.deref();
                        if (!el || !el.isConnected) {
                            stale.push(refId);
                            weakRefMap.delete(refId);
                        }
                    } else {
                        stale.push(refId);
                    }
                }
            });
            weakRefMap.forEach(function(weak, rid) {
                if (!weak.deref()) {
                    weakRefMap.delete(rid);
                    stale.push(rid);
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
                try { document.body.matches(query.css); } catch(e) {
                    return { error: 'invalid CSS selector: ' + query.css + ' — ' + e.message };
                }
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
                var existingRef = null;
                refMap.forEach(function(el, refId) {
                    if (el === node) existingRef = refId;
                });
                var ref_id = existingRef || registerRef(node);
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
                    text: (node.textContent || '').trim().substring(0, 100),
                    bounds: { x: Math.round(rect.x), y: Math.round(rect.y), width: Math.round(rect.width), height: Math.round(rect.height) },
                    visible: vis,
                    enabled: !node.disabled,
                    value: node.value || null
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

        getIpcLog: function(limit) {
            var ipcPrefix = 'http://ipc.localhost/';
            var victauriPrefix = 'plugin%3Avictauri%7C';
            var entries = [];
            for (var i = 0; i < networkLog.length; i++) {
                var n = networkLog[i];
                if (n.url.indexOf(ipcPrefix) !== 0) continue;
                var raw = n.url.substring(ipcPrefix.length);
                if (raw.indexOf(victauriPrefix) === 0) continue;
                var command;
                try { command = decodeURIComponent(raw); } catch(e) { command = raw; }
                entries.push({
                    id: n.id,
                    command: command,
                    args: n.request_args || {},
                    timestamp: n.timestamp,
                    status: n.status === 200 ? 'ok' : (n.status === 'pending' ? 'pending' : 'error'),
                    duration_ms: n.duration_ms,
                    result: n.response_body || null,
                    error: n.status !== 200 && n.status !== 'pending' ? 'HTTP ' + n.status : null,
                });
            }
            if (limit) return entries.slice(-limit);
            return entries;
        },

        clearIpcLog: function() {
            var ipcPrefix = 'http://ipc.localhost/';
            for (var i = networkLog.length - 1; i >= 0; i--) {
                if (networkLog[i].url.indexOf(ipcPrefix) === 0) networkLog.splice(i, 1);
            }
        },

        waitForIpcComplete: function(timeoutMs) {
            var log = window.__VICTAURI__.getIpcLog();
            if (log.length > 0) {
                var last = log[log.length - 1];
                if (last.duration_ms !== null && last.duration_ms !== undefined && last.result !== null) {
                    return Promise.resolve(true);
                }
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
            if (since) return consoleLogs.filter(function(l) { return l.timestamp >= since; });
            return consoleLogs;
        },

        clearConsoleLogs: function() {
            consoleLogs.length = 0;
        },

        // ── Mutations ────────────────────────────────────────────────────────

        getMutationLog: function(since) {
            if (since) return mutationLog.filter(function(m) { return m.timestamp >= since; });
            return mutationLog;
        },

        clearMutationLog: function() {
            mutationLog.length = 0;
        },

        // ── Network ──────────────────────────────────────────────────────────

        getNetworkLog: function(filter, limit) {
            var log = networkLog;
            if (filter) {
                log = log.filter(function(e) { return e.url.indexOf(filter) !== -1; });
            }
            if (limit) log = log.slice(-limit);
            return log;
        },

        clearNetworkLog: function() {
            networkLog.length = 0;
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
            return navigationLog;
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
            return dialogLog;
        },

        clearDialogLog: function() {
            dialogLog.length = 0;
        },

        setDialogAutoResponse: function(type, action, text) {
            dialogAutoResponses[type] = { action: action, text: text };
            return { ok: true };
        },

        // ── Combined Event Stream ────────────────────────────────────────────

        getEventStream: function(since) {
            var events = [];
            var ts = since || 0;

            consoleLogs.forEach(function(l) {
                if (l.timestamp >= ts) {
                    events.push({ type: 'console', level: l.level, message: l.message, timestamp: l.timestamp });
                }
            });

            mutationLog.forEach(function(m) {
                if (m.timestamp >= ts) {
                    events.push({ type: 'dom_mutation', count: m.count, timestamp: m.timestamp });
                }
            });

            var ipcPrefix = 'http://ipc.localhost/';
            var victauriPrefix = 'plugin%3Avictauri%7C';
            networkLog.forEach(function(n) {
                if (n.timestamp >= ts && n.url.indexOf(ipcPrefix) === 0) {
                    var raw = n.url.substring(ipcPrefix.length);
                    if (raw.indexOf(victauriPrefix) === 0) return;
                    var cmd; try { cmd = decodeURIComponent(raw); } catch(e) { cmd = raw; }
                    events.push({ type: 'ipc', command: cmd, status: n.status === 200 ? 'ok' : (n.status === 'pending' ? 'pending' : 'error'), duration_ms: n.duration_ms, timestamp: n.timestamp });
                }
            });

            networkLog.forEach(function(n) {
                if (n.timestamp >= ts) {
                    events.push({ type: 'network', method: n.method, url: n.url, status: n.status, duration_ms: n.duration_ms, timestamp: n.timestamp });
                }
            });

            navigationLog.forEach(function(n) {
                if (n.timestamp >= ts) {
                    events.push({ type: 'navigation', url: n.url, nav_type: n.type, timestamp: n.timestamp });
                }
            });

            interactionLog.forEach(function(i) {
                if (i.timestamp >= ts) {
                    events.push({ type: 'dom_interaction', action: i.action, selector: i.selector, value: i.value, timestamp: i.timestamp });
                }
            });

            events.sort(function(a, b) { return a.timestamp - b.timestamp; });
            return events;
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
                        var text = root.innerText || '';
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
                        met = networkLog.filter(function(n) { return n.url.indexOf('http://ipc.localhost/') === 0; }).every(function(n) { return n.status !== 'pending'; });
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
                for (var i = 0; i < important.length; i++) {
                    var v = computed.getPropertyValue(important[i]);
                    if (v && v !== '' && v !== 'none' && v !== 'normal' && v !== 'auto' && v !== '0px' && v !== 'rgba(0, 0, 0, 0)') {
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
                    transfer_size: nav.transferSize,
                    encoded_body_size: nav.encodedBodySize,
                    decoded_body_size: nav.decodedBodySize,
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

            // Paint timing
            var paints = performance.getEntriesByType('paint');
            result.paint = {};
            for (var i = 0; i < paints.length; i++) {
                result.paint[paints[i].name] = Math.round(paints[i].startTime);
            }

            // Memory (Chrome/Edge)
            if (performance.memory) {
                result.js_heap = {
                    used_mb: Math.round(performance.memory.usedJSHeapSize / 1048576 * 100) / 100,
                    total_mb: Math.round(performance.memory.totalJSHeapSize / 1048576 * 100) / 100,
                    limit_mb: Math.round(performance.memory.jsHeapSizeLimit / 1048576 * 100) / 100,
                };
            }

            // Long tasks (if PerformanceObserver captured any)
            if (longTasks.length > 0) {
                result.long_tasks = {
                    count: longTasks.length,
                    total_ms: Math.round(longTasks.reduce(function(s, t) { return s + t.duration; }, 0)),
                    worst_ms: Math.round(Math.max.apply(null, longTasks.map(function(t) { return t.duration; }))),
                };
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
    };

    try {
        Object.freeze(window.__VICTAURI__);
        Object.defineProperty(window, '__VICTAURI__', {
            value: window.__VICTAURI__,
            configurable: false,
            writable: false,
        });
    } catch(e) {}

    // ── Accessibility Helpers ────────────────────────────────────────────────

    function describeEl(el) {
        var s = '<' + el.tagName.toLowerCase();
        if (el.id) s += ' id="' + el.id + '"';
        if (el.className && typeof el.className === 'string') {
            var cls = el.className.trim();
            if (cls) s += ' class="' + cls.substring(0, 50) + '"';
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
            || (node.tagName === 'BUTTON' ? node.textContent.trim().substring(0, 80) : null)
            || (node.tagName === 'A' ? node.textContent.trim().substring(0, 80) : null);

        var element = {
            ref_id: ref_id,
            tag: node.tagName.toLowerCase(),
            role: role,
            name: name,
            text: getDirectText(node),
            value: (node.tagName === 'INPUT' && (node.getAttribute('type') || '').toLowerCase() === 'password') ? '[REDACTED]' : (node.value || null),
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

        return element;
    }

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

        var line = indent + '[' + ref_id + '] ';

        if (role && role !== tag) {
            line += role;
        } else {
            line += tag;
        }

        if (name) {
            line += ' "' + name.substring(0, 60) + '"';
        } else if (text && text.length <= 60) {
            line += ' "' + text + '"';
        } else if (text) {
            line += ' "' + text.substring(0, 57) + '..."';
        }

        if (node.disabled) line += ' [disabled]';
        if (node.value) {
            var isPassword = node.tagName === 'INPUT' && (node.getAttribute('type') || '').toLowerCase() === 'password';
            line += ' value=' + JSON.stringify(isPassword ? '[REDACTED]' : node.value.substring(0, 40));
        }

        var testId = node.getAttribute('data-testid');
        if (testId) line += ' @' + testId;

        var type = node.getAttribute('type');
        if (type && tag === 'input') line += ' type=' + type;

        var href = node.getAttribute('href');
        if (href && tag === 'a') line += ' href=' + href.substring(0, 60);

        var result = line + '\n';

        for (var c = 0; c < node.children.length; c++) {
            result += walkDomCompact(node.children[c], depth + 1);
        }

        if (node.shadowRoot) {
            for (var s = 0; s < node.shadowRoot.children.length; s++) {
                result += walkDomCompact(node.shadowRoot.children[s], depth + 1);
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
        return text.length > 0 ? text.substring(0, 200) : null;
    }

    // ── Console Hooking ──────────────────────────────────────────────────────

    var originalConsole = {
        log: console.log, warn: console.warn,
        error: console.error, info: console.info, debug: console.debug
    };

    var CTRL_RE = /[\x00-\x08\x0B\x0C\x0E-\x1F\x7F\x1B]/g;

    function hookConsole(level) {
        console[level] = function() {
            var args = Array.prototype.slice.call(arguments);
            var msg = args.map(String).join(' ').replace(CTRL_RE, '');
            consoleLogs.push({ level: level, message: msg, timestamp: Date.now() });
            if (consoleLogs.length > CAP_CONSOLE) consoleLogs.shift();
            originalConsole[level].apply(console, args);
        };
    }

    hookConsole('log');
    hookConsole('warn');
    hookConsole('error');
    hookConsole('info');
    hookConsole('debug');

    // ── Global Error Capture ────────────────────────────────────────────────

    window.addEventListener('error', function(e) {
        var msg = e.message || 'Unknown error';
        if (e.filename) msg += ' at ' + e.filename + ':' + e.lineno + ':' + e.colno;
        consoleLogs.push({ level: 'error', message: ('[uncaught] ' + msg).replace(CTRL_RE, ''), timestamp: Date.now() });
        if (consoleLogs.length > CAP_CONSOLE) consoleLogs.shift();
    });

    window.addEventListener('unhandledrejection', function(e) {
        var msg = e.reason ? (e.reason.message || String(e.reason)) : 'Unhandled promise rejection';
        consoleLogs.push({ level: 'error', message: ('[unhandled rejection] ' + msg).replace(CTRL_RE, ''), timestamp: Date.now() });
        if (consoleLogs.length > CAP_CONSOLE) consoleLogs.shift();
    });

    // ── Interaction Observer (for record mode) ────────────────────────────────

    function bestSelector(el) {
        if (el.dataset && el.dataset.testid) return '[data-testid="' + el.dataset.testid + '"]';
        if (el.id) return '#' + el.id;
        if (el.getAttribute && el.getAttribute('role')) {
            var role = el.getAttribute('role');
            var text = (el.textContent || '').trim().substring(0, 50);
            if (text) return '[role="' + role + '"]:has-text("' + text + '")';
            return '[role="' + role + '"]';
        }
        var tag = (el.tagName || 'div').toLowerCase();
        var text = (el.textContent || '').trim().substring(0, 50);
        if (text && ['button', 'a', 'label', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'span'].indexOf(tag) !== -1) {
            return tag + ':has-text("' + text + '")';
        }
        if (el.name) return tag + '[name="' + el.name + '"]';
        if (el.className && typeof el.className === 'string') {
            var cls = el.className.trim().split(/\s+/).slice(0, 2).join('.');
            if (cls) return tag + '.' + cls;
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

    (function interceptNetwork() {
        // fetch
        var origFetch = window.fetch;
        if (origFetch) {
            window.fetch = function(input, init) {
                var id = ++networkCounter;
                var url = typeof input === 'string' ? input : (input && input.url ? input.url : String(input));
                var method = (init && init.method) || (input && input.method) || 'GET';
                var isIpc = url.indexOf('http://ipc.localhost/') === 0;
                var isVictauriInternal = isIpc && url.indexOf('plugin%3Avictauri%7C') !== -1;
                var entry = { id: id, method: method.toUpperCase(), url: url, timestamp: Date.now(), status: 'pending', duration_ms: null };

                if (isIpc && !isVictauriInternal && init && init.body && window.__VICTAURI__._captureIpcBodies !== false) {
                    try {
                        var bodyStr = typeof init.body === 'string' ? init.body : null;
                        if (bodyStr) {
                            var parsed = JSON.parse(bodyStr);
                            entry.request_args = parsed;
                        }
                    } catch(e) {}
                }

                if (!isVictauriInternal) {
                    networkLog.push(entry);
                    if (networkLog.length > CAP_NETWORK) networkLog.shift();
                }

                return origFetch.call(this, input, init).then(function(response) {
                    entry.status = response.status;
                    entry.status_text = response.statusText;
                    entry.duration_ms = Date.now() - entry.timestamp;

                    if (isIpc) {
                        if (window.__VICTAURI__._captureIpcBodies !== false) {
                            var cloned = response.clone();
                            cloned.text().then(function(text) {
                                try { entry.response_body = JSON.parse(text); } catch(e) { entry.response_body = text; }
                            }).catch(function() {}).then(function() {
                                for (var w = ipcWaiters.length - 1; w >= 0; w--) {
                                    ipcWaiters[w]();
                                }
                                ipcWaiters.length = 0;
                            });
                        } else {
                            for (var w = ipcWaiters.length - 1; w >= 0; w--) {
                                ipcWaiters[w]();
                            }
                            ipcWaiters.length = 0;
                        }
                    }

                    return response;
                }, function(err) {
                    entry.status = 'error';
                    entry.error = String(err);
                    entry.duration_ms = Date.now() - entry.timestamp;
                    for (var w = ipcWaiters.length - 1; w >= 0; w--) {
                        ipcWaiters[w]();
                    }
                    ipcWaiters.length = 0;
                    throw err;
                });
            };
        }

        // XMLHttpRequest
        var origOpen = XMLHttpRequest.prototype.open;
        var origSend = XMLHttpRequest.prototype.send;
        XMLHttpRequest.prototype.open = function(method, url) {
            this.__victauri_net = { method: method, url: url };
            return origOpen.apply(this, arguments);
        };
        XMLHttpRequest.prototype.send = function() {
            if (this.__victauri_net) {
                var isVictauriInternal = this.__victauri_net.url.indexOf('plugin%3Avictauri%7C') !== -1
                    || this.__victauri_net.url.indexOf('plugin:victauri|') !== -1;
                if (isVictauriInternal) {
                    return origSend.apply(this, arguments);
                }
                var id = ++networkCounter;
                var entry = {
                    id: id,
                    method: this.__victauri_net.method.toUpperCase(),
                    url: this.__victauri_net.url,
                    timestamp: Date.now(),
                    status: 'pending',
                    duration_ms: null,
                };
                networkLog.push(entry);
                if (networkLog.length > CAP_NETWORK) networkLog.shift();
                var self = this;
                this.addEventListener('load', function() {
                    entry.status = self.status;
                    entry.status_text = self.statusText;
                    entry.duration_ms = Date.now() - entry.timestamp;
                });
                this.addEventListener('error', function() {
                    entry.status = 'error';
                    entry.duration_ms = Date.now() - entry.timestamp;
                });
            }
            return origSend.apply(this, arguments);
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
        });
        window.addEventListener('hashchange', function(e) {
            navigationLog.push({ url: window.location.href, timestamp: Date.now(), type: 'hashchange', old_url: e.oldURL });
        });
    })();

    // ── Dialog Capture ───────────────────────────────────────────────────────

    var dialogAutoResponses = { alert: { action: 'accept' }, confirm: { action: 'accept' }, prompt: { action: 'accept', text: '' } };

    // ── Resource Cleanup ────────────────────────────────────────────────────

    window.addEventListener('pagehide', function() {
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

    // Signal to the Rust backend that the JS bridge is fully initialized.
    try {
        window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {
            id: '__victauri_bridge_ready__',
            result: ''
        });
    } catch(e) {}
})();
"#;
