import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createBridgeEnv } from './setup.js';

// Audit #2 / A4: a page script can dispatch __victauri_command on the shared window, but
// the MAIN-world bridge only honours commands carrying a valid HMAC keyed by the secret
// nonce. The nonce is established with the ISOLATED relay before page scripts run and is
// NEVER placed on a command/response event, so a page cannot compute a valid MAC.
//
// setup.js mirrors the real relay: it auto-signs only commands a test dispatches with NO
// `mac` AND NO `nonce`. A command that supplies its own `mac`/`nonce` is treated as
// page-originated and left UNSIGNED, so these tests prove the bridge rejects anything that
// lacks a relay-issued MAC.
describe('bridge command authentication (audit #2 / A4)', () => {
  let env;
  beforeEach(() => {
    env = createBridgeEnv();
  });
  afterEach(() => {
    env.dom.window.close();
  });

  // Dispatch a command "as a page" — i.e. with attacker-controlled credentials that the
  // harness will NOT auto-sign. Resolves with the bridge's response, or null if ignored.
  function dispatchAsPage(detail) {
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

  it('ignores a command carrying a forged MAC', async () => {
    const resp = await dispatchAsPage({ id: 'forge1', method: 'snapshot', args: {}, mac: 'deadbeef'.repeat(8) });
    expect(resp).toBeNull();
  });

  it('ignores a command with an empty MAC', async () => {
    // An explicit (empty) mac is non-null, so the harness leaves it unsigned.
    const resp = await dispatchAsPage({ id: 'forge2', method: 'snapshot', args: {}, mac: '' });
    expect(resp).toBeNull();
  });

  it('ignores a command that carries only a (stolen) nonce but no valid MAC', async () => {
    // Even if a page somehow obtained the nonce, putting it on the command does NOT
    // authenticate it — only a valid HMAC does, and the page cannot compute one.
    const resp = await dispatchAsPage({ id: 'forge3', method: 'snapshot', args: {}, nonce: env.nonce, mac: 'deadbeef'.repeat(8) });
    expect(resp).toBeNull();
  });

  it('honours a command the relay has authenticated', async () => {
    // No mac/nonce => setup.js signs it with the shared nonce, exactly as the real
    // ISOLATED relay does for a genuine SW-issued command.
    const resp = await new Promise((resolve) => {
      const id = 'ok1';
      const handler = (e) => {
        if (e.detail && e.detail.id === id) {
          env.window.removeEventListener('__victauri_response', handler);
          resolve(e.detail);
        }
      };
      env.window.addEventListener('__victauri_response', handler);
      env.window.dispatchEvent(new env.window.CustomEvent('__victauri_command', {
        detail: { id, method: 'snapshot', args: {} },
      }));
      setTimeout(() => {
        env.window.removeEventListener('__victauri_response', handler);
        resolve(null);
      }, 200);
    });
    expect(resp).not.toBeNull();
    expect(resp.type).toBe('result');
    // The bridge signs its responses so the relay can reject page-forged ones (audit A4).
    expect(typeof resp.mac).toBe('string');
  });
});

// Audit #2 (the re-announce leak): an earlier fix generated the nonce in the MAIN world
// and re-broadcast it whenever ANY page-triggerable `__victauri_handshake_req` fired.
// Because MAIN shares the page's window, a hostile page could fire that request, capture
// the re-announced nonce, and then drive the privileged bridge. The fixed design generates
// the nonce in the ISOLATED relay and delivers it via a SINGLE-SHOT responder that is
// already spent by the legitimate document_start pull — so a page can never re-elicit it.
// This is the FIRST line of defence: the page never learns the nonce, so it can never
// compute a MAC even if the algorithm were known.
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
      // The page captured nothing (stolen === null). It has no nonce and cannot compute a
      // MAC, so it dispatches a command with a bogus mac — which the bridge must ignore.
      env.window.dispatchEvent(new env.window.CustomEvent('__victauri_command', {
        detail: { id, method: 'snapshot', args: {}, nonce: stolen == null ? '' : stolen, mac: 'deadbeef'.repeat(8) },
      }));
      setTimeout(() => {
        env.window.removeEventListener('__victauri_response', handler);
        resolve(null);
      }, 150);
    });
    expect(resp).toBeNull();
  });
});
