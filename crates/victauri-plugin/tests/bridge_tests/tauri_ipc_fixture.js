// Test fixture: Tauri 2's webview-side IPC, as injected before any plugin init script.
//
// The IIFEs marked "verbatim" are Tauri 2.11.5's `scripts/core.js` and
// `scripts/ipc-protocol.js` (identical in 2.12.4), with their `__TEMPLATE_*__` / `__RAW_*__`
// placeholders filled in, plus the `brownfield` branch of `scripts/ipc.js` and wry's
// `window.ipc` (webview2/mod.rs). Copyright 2019-2024 Tauri Programme within The Commons
// Conservancy, Apache-2.0 OR MIT.
//
// The Rust side is emulated by `__vtestBackend`: the custom-protocol fetch answers like
// tauri's `ipc::protocol::get` (HTTP 200 + `Tauri-Response` ok|error), and `window.ipc
// .postMessage` answers like `handle_ipc_message` on the fallback path (an async eval of
// `window.__TAURI_INTERNALS__.runCallback(id, value)`).
//
// Configure BEFORE this script runs with `window.__vtestTauriConfig = { os, failCustomProtocol,
// hideCallbacks }`: `os` 'windows' (default) | 'linux' | 'android'; `failCustomProtocol`
// makes every custom-protocol fetch reject (blocked scheme / CSP); `hideCallbacks` omits the
// `__TAURI_INTERNALS__.callbacks` debug property (a hypothetical future Tauri).
(function () {
  var cfg = window.__vtestTauriConfig || {};
  var OS = cfg.os || 'windows';
  var INVOKE_KEY = 'vtest-invoke-key-0123456789';

  var backend = (window.__vtestBackend = {
    fetched: [],
    posted: [],
    badKey: 0,
    failCustomProtocol: !!cfg.failCustomProtocol,
  });
  function runCommand(cmd, args) {
    switch (cmd) {
      case 'greet':
        return { ok: true, value: 'Hello, ' + (args && args.name) + '!' };
      case 'add':
        return { ok: true, value: { sum: args.a + args.b } };
      case 'fail':
        return { ok: false, value: 'boom' };
      case 'big':
        return { ok: true, value: new Array(70001).join('x') };
      case 'plugin:victauri|victauri_eval_callback':
        return { ok: true, value: null };
      default:
        return { ok: false, value: 'command ' + cmd + ' not found' };
    }
  }

  Object.defineProperty(window, '__TAURI_INTERNALS__', { value: {} });
  Object.defineProperty(window.__TAURI_INTERNALS__, '__TAURI_PATTERN__', {
    value: Object.freeze({ pattern: 'brownfield' }),
  });

  // wry (webview2/mod.rs): a frozen, non-configurable `window.ipc`.
  Object.defineProperty(window, 'ipc', {
    value: Object.freeze({
      postMessage: function (s) {
        var m = JSON.parse(s);
        if (m.__TAURI_INVOKE_KEY__ !== INVOKE_KEY) {
          backend.badKey++;
          return;
        }
        backend.posted.push({ cmd: m.cmd, blocked: !!(m.options && m.options.customProtocolIpcBlocked) });
        var r = runCommand(m.cmd, m.payload);
        setTimeout(function () {
          window.__TAURI_INTERNALS__.runCallback(r.ok ? m.callback : m.error, r.value);
        }, 5);
      },
    }),
  });

  // The custom protocol (tauri ipc::protocol::get), behind the page's fetch.
  var prevFetch = window.fetch;
  window.fetch = function (input, init) {
    var url = typeof input === 'string' ? input : String(input);
    var m = /^(?:http:\/\/ipc\.localhost|ipc:\/\/localhost)\/(.*)$/.exec(url);
    if (!m) return prevFetch.apply(this, arguments);
    if (backend.failCustomProtocol) {
      return Promise.reject(new TypeError('Failed to fetch'));
    }
    var cmd = decodeURIComponent(m[1]);
    var headers = init && init.headers;
    if (!headers || headers.get('Tauri-Invoke-Key') !== INVOKE_KEY) {
      return Promise.reject(new TypeError('bad invoke key'));
    }
    backend.fetched.push(cmd);
    var body = init.body ? JSON.parse(init.body) : {};
    var r = runCommand(cmd, body);
    return Promise.resolve(
      new Response(JSON.stringify(r.value), {
        status: 200,
        headers: { 'Content-Type': 'application/json', 'Tauri-Response': r.ok ? 'ok' : 'error' },
      }),
    );
  };

  // ── scripts/ipc.js, brownfield pattern (verbatim branch) ───────────────────
  Object.defineProperty(window.__TAURI_INTERNALS__, 'ipc', {
    value: Object.freeze(function (message) {
      window.__TAURI_INTERNALS__.postMessage(message);
    }),
  });

  // ── scripts/core.js (verbatim) ──────────────────────────────────────────────
  ;(function () {
    function uid() {
      return window.crypto.getRandomValues(new Uint32Array(1))[0]
    }

    const osName = OS
    const protocolScheme = 'http'

    Object.defineProperty(window.__TAURI_INTERNALS__, 'convertFileSrc', {
      value: function (filePath, protocol = 'asset') {
        const path = encodeURIComponent(filePath)
        return osName === 'windows' || osName === 'android'
          ? `${protocolScheme}://${protocol}.localhost/${path}`
          : `${protocol}://localhost/${path}`
      }
    })

    const callbacks = new Map()

    function registerCallback(callback, once) {
      const identifier = uid()
      callbacks.set(identifier, (data) => {
        if (once) {
          unregisterCallback(identifier)
        }
        return callback && callback(data)
      })
      return identifier
    }

    function unregisterCallback(id) {
      callbacks.delete(id)
    }

    function runCallback(id, data) {
      const callback = callbacks.get(id)
      if (callback) {
        callback(data)
      } else {
        console.warn(
          `[TAURI] Couldn't find callback id ${id}. This might happen when the app is reloaded while Rust is running an asynchronous operation.`
        )
      }
    }

    Object.defineProperty(window.__TAURI_INTERNALS__, 'transformCallback', {
      value: registerCallback
    })

    Object.defineProperty(window.__TAURI_INTERNALS__, 'unregisterCallback', {
      value: unregisterCallback
    })

    Object.defineProperty(window.__TAURI_INTERNALS__, 'runCallback', {
      value: runCallback
    })

    if (!cfg.hideCallbacks) {
      // This is just for the debugging purposes
      Object.defineProperty(window.__TAURI_INTERNALS__, 'callbacks', {
        value: callbacks
      })
    }

    const ipcQueue = []
    let isWaitingForIpc = false

    function waitForIpc() {
      if ('ipc' in window.__TAURI_INTERNALS__) {
        for (const action of ipcQueue) {
          action()
        }
      } else {
        setTimeout(waitForIpc, 50)
      }
    }

    Object.defineProperty(window.__TAURI_INTERNALS__, 'invoke', {
      value: function (cmd, payload = {}, options) {
        return new Promise(function (resolve, reject) {
          const callback = registerCallback((r) => {
            resolve(r)
            unregisterCallback(error)
          }, true)
          const error = registerCallback((e) => {
            reject(e)
            unregisterCallback(callback)
          }, true)

          const action = () => {
            window.__TAURI_INTERNALS__.ipc({
              cmd,
              callback,
              error,
              payload,
              options
            })
          }
          if ('ipc' in window.__TAURI_INTERNALS__) {
            action()
          } else {
            ipcQueue.push(action)
            if (!isWaitingForIpc) {
              waitForIpc()
              isWaitingForIpc = true
            }
          }
        })
      }
    })
  })()

  // ── scripts/ipc-protocol.js (verbatim) ──────────────────────────────────────
  ;(function () {
    const __TAURI_INVOKE_KEY__ = INVOKE_KEY

    const processIpcMessage = (function (message) {
      if (
        message instanceof ArrayBuffer
        || ArrayBuffer.isView(message)
        || Array.isArray(message)
      ) {
        return {
          contentType: 'application/octet-stream',
          data: message
        }
      } else {
        const data = JSON.stringify(message, (_k, val) => {
          const SERIALIZE_TO_IPC_FN = '__TAURI_TO_IPC_KEY__'

          if (val instanceof Map) {
            return Object.fromEntries(val.entries())
          } else if (val instanceof Uint8Array) {
            return Array.from(val)
          } else if (val instanceof ArrayBuffer) {
            return Array.from(new Uint8Array(val))
          } else if (
            typeof val === 'object'
            && val !== null
            && SERIALIZE_TO_IPC_FN in val
          ) {
            return val[SERIALIZE_TO_IPC_FN]()
          } else {
            return val
          }
        })

        return {
          contentType: 'application/json',
          data
        }
      }
    })
    const osName = OS
    const fetchChannelDataCommand = 'plugin:__TAURI_CHANNEL__|fetch'
    let customProtocolIpcFailed = false

    // on Android we never use it because Android does not have support to reading the request body
    const canUseCustomProtocol = osName !== 'android'

    function sendIpcMessage(message) {
      const { cmd, callback, error, payload, options } = message

      if (
        !customProtocolIpcFailed
        && (canUseCustomProtocol || cmd === fetchChannelDataCommand)
      ) {
        const { contentType, data } = processIpcMessage(payload)

        const headers = new Headers((options && options.headers) || {})
        headers.set('Content-Type', contentType)
        headers.set('Tauri-Callback', callback)
        headers.set('Tauri-Error', error)
        headers.set('Tauri-Invoke-Key', __TAURI_INVOKE_KEY__)

        fetch(window.__TAURI_INTERNALS__.convertFileSrc(cmd, 'ipc'), {
          method: 'POST',
          body: data,
          headers
        })
          .then((response) => {
            const callbackId =
              response.headers.get('Tauri-Response') === 'ok' ? callback : error
            // we need to split here because on Android the content-type gets duplicated
            switch ((response.headers.get('content-type') || '').split(',')[0]) {
              case 'application/json':
                return response.json().then((r) => [callbackId, r])
              case 'text/plain':
                return response.text().then((r) => [callbackId, r])
              default:
                return response.arrayBuffer().then((r) => [callbackId, r])
            }
          })
          .then(
            ([callbackId, data]) => {
              window.__TAURI_INTERNALS__.runCallback(callbackId, data)
            },
            (e) => {
              console.warn(
                'IPC custom protocol failed, Tauri will now use the postMessage interface instead',
                e
              )
              // failed to use the custom protocol IPC (either the webview blocked a custom protocol or it was a CSP error)
              // so we need to fallback to the postMessage interface
              customProtocolIpcFailed = true
              sendIpcMessage(message)
            }
          )
      } else {
        // otherwise use the postMessage interface
        const { data } = processIpcMessage({
          cmd,
          callback,
          error,
          options: {
            ...options,
            customProtocolIpcBlocked: customProtocolIpcFailed
          },
          payload,
          __TAURI_INVOKE_KEY__
        })
        window.ipc.postMessage(data)
      }
    }

    Object.defineProperty(window.__TAURI_INTERNALS__, 'postMessage', {
      value: sendIpcMessage
    })
  })()
})();
