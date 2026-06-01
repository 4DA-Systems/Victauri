import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createBridgeEnv } from './setup.js';

// Audit #2: a page script can dispatch __victauri_command, but without the secret
// nonce (established with the ISOLATED relay before page scripts run) the MAIN-world
// bridge must ignore it. setup.js only auto-stamps the nonce onto nonce-LESS events,
// so supplying an explicit wrong nonce simulates a hostile page.
describe('bridge command provenance (audit #2)', () => {
  let env;
  beforeEach(() => {
    env = createBridgeEnv();
  });
  afterEach(() => {
    env.dom.window.close();
  });

  function dispatchRaw(detail) {
    return new Promise((resolve) => {
      const handler = (e) => {
        if (e.detail && e.detail.id === detail.id) {
          env.window.removeEventListener('__victauri_response', handler);
          resolve(e.detail);
        }
      };
      env.window.addEventListener('__victauri_response', handler);
      env.window.dispatchEvent(new env.window.CustomEvent('__victauri_command', { detail }));
      setTimeout(() => {
        env.window.removeEventListener('__victauri_response', handler);
        resolve(null); // no response within the window => command was ignored
      }, 150);
    });
  }

  it('ignores a command carrying a forged nonce', async () => {
    const resp = await dispatchRaw({ id: 'forge1', method: 'snapshot', args: {}, nonce: 'forged' });
    expect(resp).toBeNull();
  });

  it('ignores a command with no nonce at all', async () => {
    // Bypass setup.js auto-stamp by giving an empty-string nonce (non-null).
    const resp = await dispatchRaw({ id: 'forge2', method: 'snapshot', args: {}, nonce: '' });
    expect(resp).toBeNull();
  });

  it('honours a command carrying the real nonce', async () => {
    const resp = await dispatchRaw({ id: 'ok1', method: 'snapshot', args: {}, nonce: env.nonce });
    expect(resp).not.toBeNull();
    expect(resp.type).toBe('result');
  });
});

// Audit #2 (the re-announce leak): an earlier fix generated the nonce in the MAIN world
// and re-broadcast it whenever ANY page-triggerable `__victauri_handshake_req` fired.
// Because MAIN shares the page's window, a hostile page could fire that request, capture
// the re-announced nonce, and then drive the privileged bridge. The fixed design generates
// the nonce in the ISOLATED relay and delivers it via a SINGLE-SHOT responder that is
// already spent by the legitimate document_start pull — so a page can never re-elicit it.
describe('bridge nonce cannot be re-elicited by a page (audit #2 re-announce leak)', () => {
  let env;
  beforeEach(() => {
    env = createBridgeEnv();
  });
  afterEach(() => {
    env.dom.window.close();
  });

  // A hostile page fires every request/announce event it can dispatch, listening on both
  // the new (`__victauri_nonce`) and legacy (`__victauri_handshake`) secret channels.
  function tryStealNonce() {
    return new Promise((resolve) => {
      let stolen = null;
      const grab = (e) => { if (e.detail && e.detail.nonce) stolen = e.detail.nonce; };
      env.window.addEventListener('__victauri_nonce', grab);
      env.window.addEventListener('__victauri_handshake', grab); // legacy leak channel
      env.window.dispatchEvent(new env.window.CustomEvent('__victauri_handshake_req'));
      env.window.dispatchEvent(new env.window.CustomEvent('__victauri_nonce_req'));
      env.window.dispatchEvent(new env.window.CustomEvent('__victauri_nonce_offer'));
      setTimeout(() => {
        env.window.removeEventListener('__victauri_nonce', grab);
        env.window.removeEventListener('__victauri_handshake', grab);
        resolve(stolen);
      }, 150);
    });
  }

  it('has already spent the single-shot nonce responder after the legitimate pull', () => {
    expect(env.relayNonceSpent()).toBe(true);
  });

  it('does not leak the nonce when a page replays the handshake/pull events', async () => {
    const stolen = await tryStealNonce();
    expect(stolen).toBeNull();
  });

  it('a page that tried to steal the nonce still cannot drive the bridge', async () => {
    const stolen = await tryStealNonce();
    const resp = await new Promise((resolve) => {
      const id = 'attack1';
      const handler = (e) => {
        if (e.detail && e.detail.id === id) {
          env.window.removeEventListener('__victauri_response', handler);
          resolve(e.detail);
        }
      };
      env.window.addEventListener('__victauri_response', handler);
      // Drive the bridge with whatever the page managed to capture. A real page that
      // captured nothing has no nonce; represent that as '' (non-null) so the harness's
      // auto-stamp convenience does not paper over the rejection. If the page HAD captured
      // the real nonce (the vulnerability), `stolen` would be passed through and accepted.
      env.window.dispatchEvent(new env.window.CustomEvent('__victauri_command', {
        detail: { id, method: 'snapshot', args: {}, nonce: stolen == null ? '' : stolen },
      }));
      setTimeout(() => {
        env.window.removeEventListener('__victauri_response', handler);
        resolve(null);
      }, 150);
    });
    expect(resp).toBeNull();
  });
});
