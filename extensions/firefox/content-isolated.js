// Content script — ISOLATED world
// Relays commands between the background script and the MAIN world content script.
// Has access to browser.runtime but NOT to page JS globals.

const api = typeof browser !== 'undefined' ? browser : chrome;

// Secret nonce shared with the MAIN-world bridge (audit #2 / A4). Generated HERE in the
// ISOLATED world (page JS cannot read this scope) and handed to MAIN exactly once, at
// document_start, before any page script runs. The responder is single-shot, so a page
// script (which only runs after document_start) can never elicit the nonce.
//
// CRITICAL (audit A4): the raw nonce is NEVER placed on any event that fires after page
// scripts exist — never on `__victauri_command` / `__victauri_response`. Those events are
// dispatched on the shared `window`, so a page could otherwise read the nonce out of the
// first real command and forge responses. They now carry only a one-way HMAC keyed by the
// nonce; a page sees MACs, never the key, so it can neither learn the secret, inject a
// command, nor forge a response.
const bridgeNonce = (() => {
    try {
        const a = new Uint8Array(16);
        crypto.getRandomValues(a);
        return Array.prototype.map.call(a, (b) => ('0' + b.toString(16)).slice(-2)).join('');
    } catch (e) {
        return null;
    }
})();
let nonceDelivered = false;
window.addEventListener('__victauri_nonce_req', () => {
    if (nonceDelivered || !bridgeNonce) return; // fail closed without a CSPRNG
    nonceDelivered = true;
    window.dispatchEvent(new CustomEvent('__victauri_nonce', { detail: { nonce: bridgeNonce } }));
});
// Announce readiness so MAIN re-requests if it loaded first. Carries no secret.
window.dispatchEvent(new CustomEvent('__victauri_nonce_offer'));

// ── Message authentication (audit A4) ─────────────────────────────────────────
// HMAC-SHA256 over the security-relevant fields, keyed by the never-broadcast nonce.
// SubtleCrypto requires a secure context (https / localhost); on a non-secure http://
// origin the authenticated channel is unavailable and the bridge fails CLOSED rather than
// fall back to forgeable id-only matching. The signed message is `JSON.stringify(parts)`
// so it is canonical and identical on both sides.
const __hasSubtle = !!bridgeNonce && typeof crypto !== 'undefined' && !!crypto.subtle;
let __macKeyPromise = null;
function __macKey() {
    if (!__macKeyPromise) {
        __macKeyPromise = crypto.subtle.importKey(
            'raw', new TextEncoder().encode(bridgeNonce),
            { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']
        );
    }
    return __macKeyPromise;
}
function __safeJson(v) {
    try { const s = JSON.stringify(v); return s === undefined ? 'null' : s; }
    catch (e) { return '"[unserializable]"'; }
}
async function __mac(parts) {
    const key = await __macKey();
    const data = new TextEncoder().encode(JSON.stringify(parts));
    const sig = await crypto.subtle.sign('HMAC', key, data);
    return Array.prototype.map.call(new Uint8Array(sig), (b) => ('0' + b.toString(16)).slice(-2)).join('');
}
function __macEq(a, b) {
    if (typeof a !== 'string' || typeof b !== 'string' || a.length !== b.length) return false;
    let d = 0;
    for (let i = 0; i < a.length; i++) d |= a.charCodeAt(i) ^ b.charCodeAt(i);
    return d === 0;
}

api.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (message.type !== 'victauri_command') return false;
    if (!__hasSubtle) {
        sendResponse({
            id: message.id, type: 'error',
            error: 'Victauri bridge disabled: the authenticated channel requires a secure context (https or localhost).'
        });
        return false;
    }

    const id = message.id;
    const method = message.method;
    const args = message.args || {};
    const argsJson = __safeJson(args);
    const argsSnapshot = JSON.parse(argsJson);

    const responsePromise = new Promise((resolve) => {
        const handler = (event) => {
            const d = event.detail;
            if (!d || d.id !== id) return;
            // Authenticate the response (audit A4): a forged `__victauri_response` with the
            // right id but no valid MAC is ignored; we wait for the real one (or timeout).
            // Snapshot every signed field before awaiting WebCrypto. The shared event detail
            // remains page-mutable after this listener returns.
            const responseId = d.id;
            const responseType = d.type;
            const responseDataJson = __safeJson(d.data);
            const responseData = d.data === undefined ? undefined : JSON.parse(responseDataJson);
            const responseError = d.error || null;
            const responseMac = d.mac;
            __mac([responseId, responseType, responseDataJson, responseError || '']).then((expected) => {
                if (!__macEq(responseMac, expected)) return;
                window.removeEventListener('__victauri_response', handler);
                resolve({
                    id: responseId, type: responseType,
                    data: responseData, error: responseError
                });
            });
        };
        window.addEventListener('__victauri_response', handler);

        setTimeout(() => {
            window.removeEventListener('__victauri_response', handler);
            resolve({ id, type: 'error', error: 'Bridge timeout (30s)' });
        }, 30000);
    });

    // Dispatch the command authenticated with a MAC; the raw nonce never goes on the wire.
    __mac([id, method, argsJson]).then((m) => {
        window.dispatchEvent(new CustomEvent('__victauri_command', {
            detail: { id, method, args: argsSnapshot, mac: m }
        }));
    });

    responsePromise.then(sendResponse);
    return true;
});

api.runtime.sendMessage({ type: 'content_script_ready', url: location.href });
