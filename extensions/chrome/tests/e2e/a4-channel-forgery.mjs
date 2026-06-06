// Real-browser regression test for audit A4 (browser-extension channel forgery).
//
// Loads the ACTUAL Victauri extension into a real Chromium instance, points it at a
// deliberately HOSTILE page, drives a command through the genuine
// service-worker -> ISOLATED relay -> MAIN bridge channel, and asserts that a malicious
// page can NEITHER:
//   (1) forge the response the agent (service worker) receives, NOR
//   (2) inject a command the bridge executes, NOR
//   (3) read the channel nonce,
// while a legitimate command still returns the correct result.
//
// This is the canonical proof that the HMAC-authenticated channel (content-isolated.js /
// content-main.js) closes A4. jsdom cannot model the ISOLATED/MAIN world split, so this
// must run in a real browser — it is intentionally NOT part of `vitest run`.
//
// Run:  node extensions/chrome/tests/e2e/a4-channel-forgery.mjs
// Needs: Playwright + a Chromium build. If Playwright is not resolvable the test SKIPS
// (exit 0) so it never breaks an environment that lacks a real browser; it exits non-zero
// ONLY on an actual security regression.
//
// Verified closed on 2026-06-07 (Chromium via Playwright 1.60).

import http from 'http';
import fs from 'fs';
import path from 'path';
import os from 'os';
import { fileURLToPath } from 'url';
import { createRequire } from 'module';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const EXT_SRC = path.resolve(__dirname, '..', '..'); // extensions/chrome

function pass(msg) { console.log('PASS  ' + msg); }
function fail(msg) { console.error('FAIL  ' + msg); process.exitCode = 1; }
function skip(msg) { console.log('SKIP  ' + msg); process.exit(0); }

// Resolve Playwright from the local install, a parent node_modules, or the npx cache.
function loadPlaywright() {
  const req = createRequire(import.meta.url);
  try { return req('playwright'); } catch { /* fall through */ }
  try { return req('@playwright/test'); } catch { /* fall through */ }
  return null;
}

const HOSTILE_HTML = `<!doctype html><html><head><meta charset="utf-8"><title>A4 hostile</title>
<script>
(function () {
  window.__hostile = { sawNonce: null, observed: [], injectedId: 'HOSTILE-INJECT' };
  // Attack 1: learn the nonce + race a forged response for any in-flight command id.
  window.addEventListener('__victauri_command', function (e) {
    var d = e && e.detail; if (!d) return;
    if (d.nonce) window.__hostile.sawNonce = d.nonce;
    if (d.id) window.dispatchEvent(new CustomEvent('__victauri_response', {
      detail: { id: d.id, type: 'result', data: 'FORGED-BY-HOSTILE-PAGE', error: null }
    }));
  }, true);
  window.addEventListener('__victauri_response', function (e) {
    var d = e && e.detail; if (d) window.__hostile.observed.push({ id: d.id, hasMac: typeof d.mac === 'string', data: d.data });
  }, true);
  // Attack 2: forge a command (bogus MAC) to make the bridge run a sensitive method.
  setTimeout(function () {
    window.dispatchEvent(new CustomEvent('__victauri_command', {
      detail: { id: window.__hostile.injectedId, method: 'getCookies', args: {}, mac: 'deadbeef'.repeat(8) }
    }));
  }, 300);
})();
</script></head><body><h1>hostile</h1></body></html>`;

// The committed manifest references icons that are generated at packaging time; load from a
// sanitized copy with the icon refs stripped so Chromium accepts the unpacked extension.
function sanitizedExtension() {
  const dst = fs.mkdtempSync(path.join(os.tmpdir(), 'vic-a4-ext-'));
  fs.cpSync(EXT_SRC, dst, { recursive: true });
  const mpath = path.join(dst, 'manifest.json');
  const m = JSON.parse(fs.readFileSync(mpath, 'utf8'));
  delete m.icons;
  if (m.action) delete m.action.default_icon;
  fs.writeFileSync(mpath, JSON.stringify(m, null, 2));
  return dst;
}

function serve() {
  return new Promise((resolve) => {
    const srv = http.createServer((req, res) => {
      res.writeHead(200, { 'content-type': 'text/html' });
      res.end(HOSTILE_HTML);
    });
    srv.listen(0, '127.0.0.1', () => resolve({ srv, port: srv.address().port }));
  });
}

async function main() {
  const pw = loadPlaywright();
  if (!pw || !pw.chromium) skip('Playwright not available — install it to run this real-browser test.');

  const ext = sanitizedExtension();
  const userDir = fs.mkdtempSync(path.join(os.tmpdir(), 'vic-a4-user-'));
  const { srv, port } = await serve();
  const url = `http://127.0.0.1:${port}/`;
  let ctx;
  try {
    ctx = await pw.chromium.launchPersistentContext(userDir, {
      headless: false, // MV3 extensions + service workers need headed (or new-headless)
      args: [`--disable-extensions-except=${ext}`, `--load-extension=${ext}`, '--no-sandbox'],
    });

    const page = await ctx.newPage();
    await page.goto(url, { waitUntil: 'load' });
    await page.waitForTimeout(1000);

    let sw = ctx.serviceWorkers()[0];
    if (!sw) { try { sw = await ctx.waitForEvent('serviceworker', { timeout: 20000 }); } catch { /* ignore */ } }
    if (!sw) sw = ctx.serviceWorkers()[0];
    if (!sw) { fail('extension service worker did not start'); return; }

    const tabId = await sw.evaluate(async (u) => {
      const tabs = await chrome.tabs.query({});
      const t = tabs.find((t) => t.url && t.url.startsWith(u));
      return t ? t.id : null;
    }, url);
    if (tabId == null) { fail('could not resolve tab id'); return; }

    // Drive a legitimate command exactly like the service worker does.
    const resp = await sw.evaluate(async ({ tabId }) => await new Promise((resolve) => {
      const to = setTimeout(() => resolve({ __timeout: true }), 8000);
      chrome.tabs.sendMessage(tabId,
        { type: 'victauri_command', id: 'verify-' + Math.random().toString(36).slice(2), method: 'eval', args: { code: '40 + 2' } },
        (r) => { clearTimeout(to); resolve(r); });
    }), { tabId });

    const hostile = await page.evaluate(() => window.__hostile);
    const data = resp && (resp.data !== undefined ? resp.data : resp);

    // (1) Response forgery must be rejected — the SW must receive the REAL result.
    if (data === '42') pass('legitimate command returns the real result (not the forgery)');
    else fail(`legitimate command did not return "42" (got ${JSON.stringify(data)})`);
    if (data === 'FORGED-BY-HOSTILE-PAGE') fail('RESPONSE FORGERY ACCEPTED — A4 regressed');
    else pass('forged response rejected');

    // (2) Command injection must be rejected — no MAC-signed response for the injected id.
    const mainExecutedInjection = (hostile.observed || []).some((r) => r.id === hostile.injectedId && r.hasMac === true);
    if (mainExecutedInjection) fail('COMMAND INJECTION EXECUTED — A4 regressed');
    else pass('forged command rejected (bridge never executed it)');

    // (3) The nonce must never appear on a page-observable event.
    if (hostile.sawNonce) fail(`NONCE LEAKED to the page (${hostile.sawNonce})`);
    else pass('nonce never leaked to the page');
  } finally {
    if (ctx) await ctx.close().catch(() => {});
    srv.close();
    try { fs.rmSync(userDir, { recursive: true, force: true }); } catch { /* ignore */ }
    try { fs.rmSync(ext, { recursive: true, force: true }); } catch { /* ignore */ }
  }

  if (process.exitCode) console.error('\nA4 channel-forgery test FAILED.');
  else console.log('\nA4 channel-forgery test PASSED — response forgery, command injection, and nonce leak all closed.');
}

main().catch((e) => { console.error('error:', e && e.message || e); process.exit(2); });
