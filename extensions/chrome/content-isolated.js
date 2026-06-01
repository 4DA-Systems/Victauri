// Content script — ISOLATED world
// Relays commands between the service worker and the MAIN world content script.
// Has access to chrome.runtime but NOT to page JS globals.

// Secret nonce shared with the MAIN-world bridge (audit #2). The nonce is GENERATED
// here in the ISOLATED world (page JS cannot read this scope) and handed to MAIN
// exactly once, during document_start — before any page script can run. The responder
// is single-shot: once the nonce has been delivered it is never offered again, so a page
// script (which only runs after document_start) can never elicit it. The handshake
// signalling events that a page *could* forge (`__victauri_nonce_offer`/`_req`) carry no
// secret, so forging them is harmless.
//
// Earlier revisions generated the nonce in MAIN and re-broadcast it on a perpetual
// `__victauri_handshake_req` listener; because MAIN shares the page's window, a page
// could fire that request and capture the nonce. This design removes that leak.
const bridgeNonce = (() => {
    try {
        const a = new Uint8Array(16);
        crypto.getRandomValues(a);
        return Array.prototype.map.call(a, (b) => ('0' + b.toString(16)).slice(-2)).join('');
    } catch (e) {
        return String(Date.now()) + Math.random().toString(36).slice(2);
    }
})();
let nonceDelivered = false;
window.addEventListener('__victauri_nonce_req', () => {
    if (nonceDelivered) return; // single-shot: never re-deliver to a late (page) requester
    nonceDelivered = true;
    window.dispatchEvent(new CustomEvent('__victauri_nonce', { detail: { nonce: bridgeNonce } }));
});
// Announce readiness so MAIN re-requests if it loaded before this relay was armed.
// Carries no secret — safe even if a page observes or forges it.
window.dispatchEvent(new CustomEvent('__victauri_nonce_offer'));

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (message.type !== 'victauri_command') return false;

    const responsePromise = new Promise((resolve) => {
        const handler = (event) => {
            if (event.detail && event.detail.id === message.id) {
                window.removeEventListener('__victauri_response', handler);
                resolve(event.detail);
            }
        };
        window.addEventListener('__victauri_response', handler);

        setTimeout(() => {
            window.removeEventListener('__victauri_response', handler);
            resolve({ id: message.id, type: 'error', error: 'Bridge timeout (30s)' });
        }, 30000);
    });

    window.dispatchEvent(new CustomEvent('__victauri_command', {
        detail: { id: message.id, method: message.method, args: message.args, nonce: bridgeNonce }
    }));

    responsePromise.then(sendResponse);
    return true;
});

chrome.runtime.sendMessage({ type: 'content_script_ready', url: location.href });
