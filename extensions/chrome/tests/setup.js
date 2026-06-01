import { readFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import { JSDOM } from 'jsdom';

const __dirname = dirname(fileURLToPath(import.meta.url));
const BRIDGE_PATH = resolve(__dirname, '..', 'content-main.js');
const bridgeSource = readFileSync(BRIDGE_PATH, 'utf8');

export function createBridgeEnv(html = '<html><head><title>Test</title></head><body></body></html>') {
  const dom = new JSDOM(html, {
    url: 'http://localhost:3000',
    pretendToBeVisual: true,
    runScripts: 'dangerously',
    resources: 'usable',
  });

  const { window } = dom;

  // jsdom doesn't have PerformanceObserver — stub it
  if (!window.PerformanceObserver) {
    window.PerformanceObserver = class {
      observe() {}
      disconnect() {}
    };
  }

  // jsdom doesn't have performance.getEntriesByType — stub minimal
  if (!window.performance.getEntriesByType) {
    window.performance.getEntriesByType = () => [];
  }

  // jsdom getBoundingClientRect returns all zeros — stub non-zero values for actionability
  const origGetBCR = window.HTMLElement.prototype.getBoundingClientRect;
  window.HTMLElement.prototype.getBoundingClientRect = function() {
    return { x: 10, y: 10, width: 100, height: 30, top: 10, right: 110, bottom: 40, left: 10 };
  };

  // jsdom doesn't have elementFromPoint — stub to return the element itself
  if (!window.document.elementFromPoint || window.document.elementFromPoint(0,0) === null) {
    window.document.elementFromPoint = function(x, y) {
      return window.document.body.firstElementChild || window.document.body;
    };
  }

  // Stub getComputedStyle to return basic values jsdom doesn't compute
  const origGetComputedStyle = window.getComputedStyle;
  window.getComputedStyle = function(el) {
    const styles = origGetComputedStyle.call(window, el);
    return new Proxy(styles, {
      get(target, prop) {
        if (prop === 'display') return target.display || 'block';
        if (prop === 'visibility') return target.visibility || 'visible';
        if (prop === 'opacity') return target.opacity || '1';
        if (prop === 'pointerEvents') return target.pointerEvents || 'auto';
        if (prop === 'color') return target.color || 'rgb(0, 0, 0)';
        if (prop === 'backgroundColor') return target.backgroundColor || 'rgb(255, 255, 255)';
        if (prop === 'fontSize') return target.fontSize || '16px';
        if (prop === 'getPropertyValue') return function(name) {
          return target.getPropertyValue(name) || '';
        };
        return target[prop];
      }
    });
  };

  // Simulate the ISOLATED relay's half of the audit-#2 handshake. The relay OWNS the
  // nonce and hands it to the MAIN bridge via a SINGLE-SHOT responder: once the bridge
  // has pulled it, the relay never re-delivers it. This mirrors the real
  // content-isolated.js and lets the provenance tests prove a page cannot re-elicit the
  // nonce after load. The responder is registered BEFORE injection so the bridge's
  // on-load pull is answered synchronously (as it is at document_start in a real browser).
  const relayNonce = (() => {
    try {
      const a = new Uint8Array(16);
      (window.crypto || globalThis.crypto).getRandomValues(a);
      return Array.prototype.map.call(a, (b) => ('0' + b.toString(16)).slice(-2)).join('');
    } catch (e) {
      return 'testnonce' + Math.random().toString(36).slice(2);
    }
  })();
  let relayNonceDelivered = false;
  window.addEventListener('__victauri_nonce_req', () => {
    if (relayNonceDelivered) return; // single-shot — never re-deliver to a late requester
    relayNonceDelivered = true;
    window.dispatchEvent(new window.CustomEvent('__victauri_nonce', { detail: { nonce: relayNonce } }));
  });

  // Inject the bridge script. On load it dispatches __victauri_nonce_req synchronously,
  // which our already-registered single-shot responder answers — exactly as the real
  // relay does at document_start, before any page script runs.
  const script = window.document.createElement('script');
  script.textContent = bridgeSource;
  window.document.head.appendChild(script);

  // Mirror the relay's readiness offer (covers bridge-loaded-first ordering).
  window.dispatchEvent(new window.CustomEvent('__victauri_nonce_offer'));

  // Simulate the ISOLATED relay: stamp the nonce onto nonce-less command events so
  // existing tests (which dispatch __victauri_command directly) keep working.
  const origDispatch = window.dispatchEvent.bind(window);
  window.dispatchEvent = function (ev) {
    if (ev && ev.type === '__victauri_command' && ev.detail && ev.detail.nonce == null && relayNonce) {
      ev = new window.CustomEvent('__victauri_command', {
        detail: Object.assign({}, ev.detail, { nonce: relayNonce }),
      });
    }
    return origDispatch(ev);
  };

  return {
    dom,
    window,
    bridge: window.__VICTAURI__,
    nonce: relayNonce,
    // True once the relay's single-shot nonce responder has fired — a page that tries to
    // re-elicit the nonce after the legitimate pull must find it spent.
    relayNonceSpent: () => relayNonceDelivered,
  };
}
