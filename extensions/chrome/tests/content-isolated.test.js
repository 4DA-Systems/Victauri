import { describe, it, expect } from 'vitest';
import { readFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(resolve(__dirname, '..', 'content-isolated.js'), 'utf8');

describe('content-isolated.js structure', () => {
  it('file is non-empty', () => {
    expect(source.length).toBeGreaterThan(100);
  });

  it('listens for messages from service worker', () => {
    expect(source).toContain('chrome.runtime.onMessage');
  });

  it('dispatches CustomEvent to MAIN world', () => {
    expect(source).toContain('__victauri_command');
    expect(source).toContain('CustomEvent');
  });

  it('listens for response from MAIN world', () => {
    expect(source).toContain('__victauri_response');
  });

  it('sends content_script_ready signal', () => {
    expect(source).toContain('content_script_ready');
  });

  it('has timeout handling', () => {
    expect(source).toContain('setTimeout');
  });

  it('uses sendResponse callback', () => {
    expect(source).toContain('sendResponse');
  });

  it('returns true for async response', () => {
    expect(source).toContain('return true');
  });

  it('fails closed instead of falling back to a predictable nonce', () => {
    expect(source).not.toContain('Math.random');
    expect(source).toContain('nonceDelivered || !bridgeNonce');
  });

  it('snapshots signed response fields before asynchronous verification', () => {
    expect(source).toContain('const responseDataJson = __safeJson(d.data)');
    expect(source).toContain('data: responseData');
  });
});
