(function() {
    'use strict';
    if (window.__VICTAURI__) return;

    var CAP_CONSOLE = 1000;
    var CAP_MUTATION = 500;
    var CAP_NETWORK = 1000;
    var CAP_NAVIGATION = 200;
    var CAP_DIALOG = 100;
    var CAP_LONG_TASKS = 100;

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
        version: '0.7.9-browser',

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
                    try { if (!el.matches(query.css)) return false; } catch(e) { return false; }
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

            var inputs = document.querySelectorAll('input, select, textarea');
            for (var i = 0; i < inputs.length; i++) {
                var inp = inputs[i];
                if (inp.type === 'hidden') continue;
                var hasLabel = false;
                if (inp.id) {
                    try { hasLabel = !!document.querySelector('label[for="' + CSS.escape(inp.id) + '"]'); }
                    catch(e) {}
                }
                var hasAria = inp.getAttribute('aria-label') || inp.getAttribute('aria-labelledby');
                var hasTitle = inp.title;
                var hasPlaceholder = inp.placeholder;
                if (!hasLabel && !hasAria && !hasTitle && !hasPlaceholder) {
                    violations.push({ rule: 'input-label', severity: 'serious', element: describeEl(inp),
                        message: 'Form input has no accessible label' });
                }
            }

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

            if (!document.documentElement.lang) {
                violations.push({ rule: 'html-lang', severity: 'serious', element: '<html>',
                    message: 'Document missing lang attribute' });
            }

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

            if (!document.title || document.title.trim() === '') {
                violations.push({ rule: 'document-title', severity: 'serious', element: '<head>',
                    message: 'Document has no title' });
            }

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

            var paints = performance.getEntriesByType('paint');
            result.paint = {};
            for (var i = 0; i < paints.length; i++) {
                result.paint[paints[i].name] = Math.round(paints[i].startTime);
            }

            if (performance.memory) {
                result.js_heap = {
                    used_mb: Math.round(performance.memory.usedJSHeapSize / 1048576 * 100) / 100,
                    total_mb: Math.round(performance.memory.totalJSHeapSize / 1048576 * 100) / 100,
                    limit_mb: Math.round(performance.memory.jsHeapSizeLimit / 1048576 * 100) / 100,
                };
            }

            if (longTasks.length > 0) {
                result.long_tasks = {
                    count: longTasks.length,
                    total_ms: Math.round(longTasks.reduce(function(s, t) { return s + t.duration; }, 0)),
                    worst_ms: Math.round(Math.max.apply(null, longTasks.map(function(t) { return t.duration; }))),
                };
            }

            result.dom = {
                elements: document.querySelectorAll('*').length,
                max_depth: (function() { var d = 0; var walk = function(el, depth) { if (depth > d) d = depth; for (var i = 0; i < el.children.length && i < 5; i++) walk(el.children[i], depth + 1); }; walk(document.body, 0); return d; })(),
                event_listeners: listenerCount,
            };

            return result;
        },

        getDiagnostics: function() {
            var diag = { warnings: [], info: {} };

            if (navigator.serviceWorker && navigator.serviceWorker.controller) {
                diag.warnings.push({
                    id: 'service-worker-active',
                    severity: 'high',
                    message: 'Active service worker detected — may intercept network requests',
                    details: { scope: navigator.serviceWorker.controller.scriptURL }
                });
            }

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

            var iframes = document.querySelectorAll('iframe');
            if (iframes.length > 0) {
                diag.warnings.push({
                    id: 'iframes-present',
                    severity: 'medium',
                    message: iframes.length + ' iframe(s) found — bridge is not injected inside iframes',
                    details: { count: iframes.length, srcs: Array.from(iframes).slice(0, 5).map(function(f) { return f.src || '(empty)'; }) }
                });
            }

            var elementCount = allEls.length;
            if (elementCount > 5000) {
                diag.warnings.push({
                    id: 'large-dom',
                    severity: 'low',
                    message: 'DOM has ' + elementCount + ' elements — dom_snapshot may be slow (>100ms)',
                    details: { count: elementCount }
                });
            }

            var cspMeta = document.querySelector('meta[http-equiv="Content-Security-Policy"]');
            if (cspMeta) {
                var cspContent = cspMeta.getAttribute('content') || '';
                diag.info.csp_meta = cspContent;
            }

            diag.info.bridge_version = window.__VICTAURI__.version;
            diag.info.user_agent = navigator.userAgent;
            diag.info.url = window.location.href;
            diag.info.dom_elements = elementCount;
            diag.info.open_shadow_roots = (function() { var c = 0; for (var i = 0; i < allEls.length; i++) { if (allEls[i].shadowRoot) c++; } return c; })();
            diag.info.event_listeners = listenerCount;
            diag.info.protocol = window.location.protocol;
            diag.info.mode = 'browser';

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

    // ── Helpers ──────────────────────────────────────────────────────────────

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
            var logEntry = { level: level, message: msg, timestamp: Date.now() };
            consoleLogs.push(logEntry);
            pushRecordingEvent('console', logEntry);
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

    // ── Interaction Observer ─────────────────────────────────────────────────

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

    // ── Network Interception ─────────────────────────────────────────────────

    (function interceptNetwork() {
        var origFetch = window.fetch;
        if (origFetch) {
            window.fetch = function(input, init) {
                var id = ++networkCounter;
                var url = typeof input === 'string' ? input : (input && input.url ? input.url : String(input));
                var method = (init && init.method) || (input && input.method) || 'GET';
                var entry = { id: id, method: method.toUpperCase(), url: url, timestamp: Date.now(), status: 'pending', duration_ms: null };

                networkLog.push(entry);
                pushRecordingEvent('network', { method: entry.method, url: entry.url });
                if (networkLog.length > CAP_NETWORK) networkLog.shift();

                return origFetch.call(this, input, init).then(function(response) {
                    entry.status = response.status;
                    entry.status_text = response.statusText;
                    entry.duration_ms = Date.now() - entry.timestamp;
                    return response;
                }, function(err) {
                    entry.status = 'error';
                    entry.error = String(err);
                    entry.duration_ms = Date.now() - entry.timestamp;
                    throw err;
                });
            };
        }

        var origOpen = XMLHttpRequest.prototype.open;
        var origSend = XMLHttpRequest.prototype.send;
        XMLHttpRequest.prototype.open = function(method, url) {
            this.__victauri_net = { method: method, url: url };
            return origOpen.apply(this, arguments);
        };
        XMLHttpRequest.prototype.send = function() {
            if (this.__victauri_net) {
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
            var navEntry = { url: window.location.href, timestamp: Date.now(), type: 'pushState' };
            navigationLog.push(navEntry);
            pushRecordingEvent('navigation', navEntry);
            if (navigationLog.length > CAP_NAVIGATION) navigationLog.shift();
            return result;
        };
        history.replaceState = function() {
            var result = origReplaceState.apply(this, arguments);
            var navEntry = { url: window.location.href, timestamp: Date.now(), type: 'replaceState' };
            navigationLog.push(navEntry);
            pushRecordingEvent('navigation', navEntry);
            if (navigationLog.length > CAP_NAVIGATION) navigationLog.shift();
            return result;
        };
        window.addEventListener('popstate', function() {
            var navEntry = { url: window.location.href, timestamp: Date.now(), type: 'popstate' };
            navigationLog.push(navEntry);
            pushRecordingEvent('navigation', navEntry);
        });
        window.addEventListener('hashchange', function(e) {
            var navEntry = { url: window.location.href, timestamp: Date.now(), type: 'hashchange', old_url: e.oldURL };
            navigationLog.push(navEntry);
            pushRecordingEvent('navigation', navEntry);
        });
    })();

    // ── Dialog Capture ───────────────────────────────────────────────────────

    var dialogAutoResponses = { alert: { action: 'accept' }, confirm: { action: 'accept' }, prompt: { action: 'accept', text: '' } };

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

    // ── Recording ─────────────────────────────────────────────────────────────

    var recordingSession = null;
    var recordingEvents = [];
    var recordingCheckpoints = [];

    function startRecording() {
        var sessionId = 'rec-' + Date.now() + '-' + Math.random().toString(36).slice(2, 8);
        recordingSession = { id: sessionId, started: Date.now() };
        recordingEvents = [];
        recordingCheckpoints = [];
        return { session_id: sessionId, started: true };
    }

    function stopRecording() {
        if (!recordingSession) return { error: 'no active recording' };
        var session = {
            session_id: recordingSession.id,
            duration_ms: Date.now() - recordingSession.started,
            events: recordingEvents,
            checkpoints: recordingCheckpoints,
        };
        recordingSession = null;
        recordingEvents = [];
        recordingCheckpoints = [];
        return session;
    }

    function recordCheckpoint(args) {
        if (!recordingSession) return { error: 'no active recording' };
        var cp = {
            checkpoint_id: 'cp-' + Date.now() + '-' + Math.random().toString(36).slice(2, 6),
            label: (args && args.label) || null,
            timestamp: Date.now(),
            event_index: recordingEvents.length,
        };
        recordingCheckpoints.push(cp);
        return { checkpoint_id: cp.checkpoint_id, created: true, event_index: cp.event_index };
    }

    function getRecordingEvents(args) {
        var since = (args && args.since) || 0;
        return recordingEvents.filter(function(e) { return e.timestamp >= since; });
    }

    function listRecordingCheckpoints() {
        return recordingCheckpoints;
    }

    function exportRecording() {
        if (!recordingSession) return { error: 'no active recording' };
        return {
            session_id: recordingSession.id,
            started: recordingSession.started,
            events: recordingEvents,
            checkpoints: recordingCheckpoints,
        };
    }

    function pushRecordingEvent(type, data) {
        if (!recordingSession) return;
        recordingEvents.push({ type: type, data: data, timestamp: Date.now() });
    }

    // ── Command Dispatch (browser extension bridge) ──────────────────────────

    var AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;

    // Provenance gate (audit #2): only honour commands carrying the secret nonce that the
    // ISOLATED relay hands us during a one-shot handshake at document_start, before any
    // page script runs. The nonce is generated in (and owned by) the ISOLATED world; we
    // pull it exactly once and keep it in this IIFE closure (page JS cannot read it). The
    // nonce is NEVER broadcast in response to a page-triggerable event, so a page can
    // dispatch __victauri_command but cannot learn the nonce and cannot drive the bridge.
    var __victauriNonce = null;
    function __victauriRequestNonce() {
        window.dispatchEvent(new CustomEvent('__victauri_nonce_req'));
    }
    window.addEventListener('__victauri_nonce', function (event) {
        if (__victauriNonce === null && event.detail && event.detail.nonce) {
            __victauriNonce = event.detail.nonce;
        }
    });
    // If ISOLATED loaded first, our initial request reaches its still-armed responder; if
    // we loaded first, its offer prompts us to re-request once it is ready. Either way the
    // pull completes synchronously during document_start, before page scripts run.
    window.addEventListener('__victauri_nonce_offer', __victauriRequestNonce);
    __victauriRequestNonce();

    // ── Message authentication (audit A4) ─────────────────────────────────────
    // HMAC-SHA256 keyed by the never-broadcast nonce. The raw nonce is NEVER placed on a
    // command/response event; only one-way MACs are, so a page that observes the shared
    // window cannot recover the key, inject a command, or forge a response. SubtleCrypto
    // needs a secure context (https / localhost); on a non-secure origin the bridge fails
    // CLOSED rather than accept forgeable id-only traffic. The signed message is
    // `JSON.stringify(parts)` so it is canonical and identical on both sides.
    var __vicHasSubtle = typeof crypto !== 'undefined' && !!crypto.subtle;
    var __vicMacKeyPromise = null;
    function __vicMacKey() {
        if (!__vicMacKeyPromise) {
            __vicMacKeyPromise = crypto.subtle.importKey(
                'raw', new TextEncoder().encode(__victauriNonce),
                { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']
            );
        }
        return __vicMacKeyPromise;
    }
    function __vicSafeJson(v) {
        try { var s = JSON.stringify(v); return s === undefined ? 'null' : s; }
        catch (e) { return '"[unserializable]"'; }
    }
    function __vicMac(parts) {
        return __vicMacKey().then(function (key) {
            var data = new TextEncoder().encode(JSON.stringify(parts));
            return crypto.subtle.sign('HMAC', key, data).then(function (sig) {
                return Array.prototype.map.call(new Uint8Array(sig),
                    function (b) { return ('0' + b.toString(16)).slice(-2); }).join('');
            });
        });
    }
    function __vicMacEq(a, b) {
        if (typeof a !== 'string' || typeof b !== 'string' || a.length !== b.length) return false;
        var d = 0;
        for (var i = 0; i < a.length; i++) d |= a.charCodeAt(i) ^ b.charCodeAt(i);
        return d === 0;
    }

    window.addEventListener('__victauri_command', function(event) {
        var detail = event.detail;
        // Fail closed if the handshake never completed or we lack a secure context.
        if (!detail || __victauriNonce === null || !__vicHasSubtle
            || !detail.id || !detail.method) return;

        var id = detail.id;
        var method = detail.method;
        var args = detail.args || {};

        // Authenticate the command (audit A4): only honour commands carrying a valid MAC
        // derived from the never-broadcast nonce. A page cannot forge this, so it can
        // neither inject commands nor make us mint authenticated responses on its behalf.
        __vicMac([id, method, __vicSafeJson(args)]).then(function (expected) {
            if (!__vicMacEq(detail.mac, expected)) return;
            try {
                var result = executeBridgeMethod(method, args);
                Promise.resolve(result).then(
                    function(data) { dispatchResponse(id, 'result', data, null); },
                    function(err) { dispatchResponse(id, 'error', null, err.message || String(err)); }
                );
            } catch (err) {
                dispatchResponse(id, 'error', null, err.message || String(err));
            }
        });
    });

    function dispatchResponse(id, type, data, error) {
        // Sign the response so the ISOLATED relay can reject a page's forged
        // `__victauri_response` (audit A4). If we lack crypto, emit nothing — the relay
        // times out rather than deliver an unauthenticated result.
        if (!__vicHasSubtle || __victauriNonce === null) return;
        __vicMac([id, type, __vicSafeJson(data), error || '']).then(function (m) {
            window.dispatchEvent(new CustomEvent('__victauri_response', {
                detail: { id: id, type: type, data: data, error: error, mac: m }
            }));
        });
    }

    function executeBridgeMethod(method, args) {
        var bridge = window.__VICTAURI__;
        switch (method) {
            case 'snapshot': return bridge.snapshot(args.format);
            case 'findElements': return bridge.findElements(args);
            case 'click': return bridge.click(args.ref_id, args.timeout_ms);
            case 'doubleClick': return bridge.doubleClick(args.ref_id, args.timeout_ms);
            case 'hover': return bridge.hover(args.ref_id, args.timeout_ms);
            case 'fill': return bridge.fill(args.ref_id, args.value, args.timeout_ms);
            case 'type': return bridge.type(args.ref_id, args.text, args.timeout_ms);
            case 'pressKey': return bridge.pressKey(args.key);
            case 'selectOption': return bridge.selectOption(args.ref_id, args.values, args.timeout_ms);
            case 'scrollTo': return bridge.scrollTo(args.ref_id, args.x, args.y, args.timeout_ms);
            case 'focusElement': return bridge.focusElement(args.ref_id, args.timeout_ms);
            case 'getConsoleLogs': return bridge.getConsoleLogs(args.since);
            case 'clearConsoleLogs': return bridge.clearConsoleLogs();
            case 'getMutationLog': return bridge.getMutationLog(args.since);
            case 'clearMutationLog': return bridge.clearMutationLog();
            case 'getNetworkLog': return bridge.getNetworkLog(args.filter, args.limit);
            case 'clearNetworkLog': return bridge.clearNetworkLog();
            case 'getLocalStorage': return bridge.getLocalStorage(args.key);
            case 'setLocalStorage': return bridge.setLocalStorage(args.key, args.value);
            case 'deleteLocalStorage': return bridge.deleteLocalStorage(args.key);
            case 'getSessionStorage': return bridge.getSessionStorage(args.key);
            case 'setSessionStorage': return bridge.setSessionStorage(args.key, args.value);
            case 'deleteSessionStorage': return bridge.deleteSessionStorage(args.key);
            case 'getCookies': return bridge.getCookies();
            case 'getNavigationLog': return bridge.getNavigationLog();
            case 'navigate': return bridge.navigate(args.url);
            case 'navigateBack': return bridge.navigateBack();
            case 'getDialogLog': return bridge.getDialogLog();
            case 'clearDialogLog': return bridge.clearDialogLog();
            case 'setDialogAutoResponse': return bridge.setDialogAutoResponse(args.type, args.action, args.text);
            case 'getEventStream': return bridge.getEventStream(args.since);
            case 'waitFor': return bridge.waitFor(args);
            case 'getStyles': return bridge.getStyles(args.ref_id, args.properties);
            case 'getBoundingBoxes': return bridge.getBoundingBoxes(args.ref_ids);
            case 'highlightElement': return bridge.highlightElement(args.ref_id, args.color, args.label);
            case 'clearHighlights': return bridge.clearHighlights();
            case 'injectCss': return bridge.injectCss(args.css);
            case 'removeInjectedCss': return bridge.removeInjectedCss();
            case 'auditAccessibility': return bridge.auditAccessibility();
            case 'getPerformanceMetrics': return bridge.getPerformanceMetrics();
            case 'getDiagnostics': return bridge.getDiagnostics();
            case 'eval': return evalInPage(args.code);
            case 'recording_start': return startRecording();
            case 'recording_stop': return stopRecording();
            case 'recording_checkpoint': return recordCheckpoint(args);
            case 'recording_get_events': return getRecordingEvents(args);
            case 'recording_list_checkpoints': return listRecordingCheckpoints();
            case 'recording_export': return exportRecording();
            default: throw new Error('Unknown bridge method: ' + method);
        }
    }

    async function evalInPage(code) {
        var trimmed = code.trim();
        var statements = ['if','for','while','do','switch','try','const','let','var','class','function','return','throw','import','export','{'];
        var needsReturn = !statements.some(function(k) { return trimmed.startsWith(k); });
        var wrapped = needsReturn ? 'return ' + trimmed : trimmed;
        var fn = new AsyncFunction(wrapped);
        var result = await fn();
        return result === undefined ? 'undefined' : JSON.stringify(result);
    }
})();
