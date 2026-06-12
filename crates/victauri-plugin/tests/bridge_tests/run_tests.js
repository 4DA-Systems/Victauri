#!/usr/bin/env node
// Victauri JS Bridge test runner.
// Usage: node run_tests.js <test-definition.json>
//
// Input JSON schema:
// {
//   "bridge_script": "...",    // The full JS bridge source to inject
//   "setup_html": "...",       // HTML to set as document body
//   "setup_js": "...",         // Optional JS to run after bridge injection
//   "tests": [{ "name": "...", "code": "..." }]
// }
//
// Each test.code is JS that returns a result value (or a Promise).
//
// Output: JSON array of { "name", "passed", "result", "error" }
// Sent as a single line on stdout, prefixed with "RESULTS:" to
// distinguish from any console output leaked by the bridge.

const fs = require("fs");
const { JSDOM } = require("jsdom");

const testDefPath = process.argv[2];
if (!testDefPath) {
  process.stderr.write("Usage: node run_tests.js <test-definition.json>\n");
  process.exit(1);
}

const testDef = JSON.parse(fs.readFileSync(testDefPath, "utf-8"));

async function run() {
  const results = [];

  for (const test of testDef.tests) {
    // Create a fresh jsdom for each test to avoid cross-contamination
    const html =
      test.setup_html ||
      testDef.setup_html ||
      "<html><head><title>Test</title></head><body></body></html>";
    const dom = new JSDOM(html, {
      url: "http://localhost/",
      pretendToBeVisual: true,
      runScripts: "dangerously",
      resources: "usable",
    });

    const window = dom.window;

    // IMPORTANT: Silence the jsdom window's console so that bridge console
    // hooks (which call originalConsole.log.apply) don't write to Node's
    // stdout and corrupt our JSON output.
    const noop = () => {};
    window.console.log = noop;
    window.console.warn = noop;
    window.console.error = noop;
    window.console.info = noop;
    window.console.debug = noop;

    // Stub getBoundingClientRect on all elements to return non-zero rects.
    // jsdom doesn't do layout so these are always zero by default.
    window.HTMLElement.prototype.getBoundingClientRect = function () {
      const tag = this.tagName || "";
      let width = 100,
        height = 30;
      if (tag === "BODY") {
        width = 1200;
        height = 800;
      } else if (tag === "DIV") {
        width = 200;
        height = 50;
      } else if (tag === "BUTTON") {
        width = 80;
        height = 32;
      } else if (tag === "INPUT") {
        width = 200;
        height = 24;
      } else if (tag === "TEXTAREA") {
        width = 300;
        height = 100;
      } else if (tag === "IMG") {
        width = 150;
        height = 150;
      } else if (tag === "SELECT") {
        width = 200;
        height = 24;
      } else if (tag === "A") {
        width = 60;
        height = 20;
      } else if (tag === "SPAN") {
        width = 50;
        height = 16;
      } else if (tag === "P") {
        width = 400;
        height = 20;
      } else if (tag === "H1" || tag === "H2" || tag === "H3") {
        width = 400;
        height = 32;
      } else if (tag === "NAV") {
        width = 800;
        height = 40;
      } else if (tag === "FORM") {
        width = 400;
        height = 200;
      } else if (tag === "UL" || tag === "OL") {
        width = 300;
        height = 100;
      } else if (tag === "LI") {
        width = 280;
        height = 24;
      }

      return {
        x: 10,
        y: 10,
        left: 10,
        top: 10,
        right: 10 + width,
        bottom: 10 + height,
        width: width,
        height: height,
        toJSON() {
          return {
            x: this.x,
            y: this.y,
            width: this.width,
            height: this.height,
            left: this.left,
            top: this.top,
            right: this.right,
            bottom: this.bottom,
          };
        },
      };
    };

    // Stub getComputedStyle to return basic visible values.
    const origGCS = window.getComputedStyle;
    window.getComputedStyle = function (el) {
      const real = origGCS.call(window, el);
      return new Proxy(real, {
        get(target, prop) {
          if (prop === "display") {
            if (el.style && el.style.display === "none") return "none";
            return target.display || "block";
          }
          if (prop === "visibility") {
            if (el.style && el.style.visibility === "hidden") return "hidden";
            return target.visibility || "visible";
          }
          if (prop === "opacity") {
            if (el.style && el.style.opacity === "0") return "0";
            return target.opacity || "1";
          }
          if (prop === "color") return target.color || "rgb(0, 0, 0)";
          if (prop === "backgroundColor")
            return target.backgroundColor || "rgba(0, 0, 0, 0)";
          if (prop === "fontSize") return target.fontSize || "16px";
          if (prop === "fontWeight") return target.fontWeight || "400";
          if (
            prop === "marginTop" ||
            prop === "marginRight" ||
            prop === "marginBottom" ||
            prop === "marginLeft"
          )
            return target[prop] || "0px";
          if (
            prop === "paddingTop" ||
            prop === "paddingRight" ||
            prop === "paddingBottom" ||
            prop === "paddingLeft"
          )
            return target[prop] || "0px";
          if (
            prop === "borderTopWidth" ||
            prop === "borderRightWidth" ||
            prop === "borderBottomWidth" ||
            prop === "borderLeftWidth"
          )
            return target[prop] || "0px";
          if (prop === "getPropertyValue") {
            return function (name) {
              const v = target.getPropertyValue(name);
              if (v) return v;
              const defaults = {
                display: "block",
                visibility: "visible",
                opacity: "1",
                color: "rgb(0, 0, 0)",
                "background-color": "rgba(0, 0, 0, 0)",
                "font-size": "16px",
                "font-weight": "400",
                "font-family": "sans-serif",
                position: "static",
                width: "auto",
                height: "auto",
              };
              return defaults[name] || "";
            };
          }
          const val = target[prop];
          if (typeof val === "function") return val.bind(target);
          return val;
        },
      });
    };

    // Stub performance APIs that jsdom lacks
    if (!window.performance.getEntriesByType) {
      window.performance.getEntriesByType = function () {
        return [];
      };
    }

    // Stub window.fetch so the bridge's network interceptor can wrap it.
    // jsdom doesn't provide fetch natively. Our stub resolves immediately
    // with a minimal Response-like object so the bridge's fetch wrapper
    // can update its network log entries.
    if (!window.fetch) {
      window.fetch = function (input, init) {
        const url =
          typeof input === "string"
            ? input
            : input && input.url
              ? input.url
              : String(input);
        // Let a test simulate Tauri's command-outcome header: a request header
        // `x-vtest-tauri-response: error` makes the response carry
        // `Tauri-Response: error` (Tauri returns HTTP 200 either way). Defaults
        // to 'ok', matching a real successful command.
        var reqHeaders = (init && init.headers) || {};
        var tauriResp = reqHeaders["x-vtest-tauri-response"] || "ok";
        var respBody =
          reqHeaders["x-vtest-body"] != null ? reqHeaders["x-vtest-body"] : "";
        // Return a resolved promise with a fake Response that supports clone()
        function makeResponse() {
          return {
            ok: true,
            status: 200,
            statusText: "OK",
            url: url,
            headers: new Map([["Tauri-Response", tauriResp]]),
            text: function () {
              return Promise.resolve(respBody);
            },
            json: function () {
              return Promise.resolve({});
            },
            clone: function () {
              return makeResponse();
            },
          };
        }
        return Promise.resolve(makeResponse());
      };
    }

    // Stub document.elementFromPoint (jsdom doesn't do layout).
    // Returns the element itself when called from within the bridge's
    // actionability check, simulating an unobstructed element.
    if (!window.document.elementFromPoint) {
      window.document.elementFromPoint = function (x, y) {
        // Return the deepest element at the approximate location.
        // For testing purposes, just return the body or first child.
        // The bridge uses this to check if element is covered.
        // Returning null means the check passes (topEl === null path).
        return null;
      };
    }

    // Polyfill innerText (jsdom doesn't support it, but the bridge uses it
    // in waitFor's getFullText helper). We approximate it with textContent.
    if (!("innerText" in window.HTMLElement.prototype)) {
      Object.defineProperty(window.HTMLElement.prototype, "innerText", {
        get: function () {
          return this.textContent;
        },
        set: function (v) {
          this.textContent = v;
        },
        configurable: true,
      });
    }

    // Stub scrollIntoView
    if (!window.HTMLElement.prototype.scrollIntoView) {
      window.HTMLElement.prototype.scrollIntoView = function () {};
    }

    // Stub window.scrollTo
    if (!window.scrollTo) {
      window.scrollTo = function () {};
    }

    // Inject the bridge script
    try {
      window.eval(testDef.bridge_script);
    } catch (e) {
      results.push({
        name: test.name,
        passed: false,
        result: null,
        error: "Bridge injection failed: " + e.message,
      });
      dom.window.close();
      continue;
    }

    // Run optional per-test setup JS
    const setupJs = test.setup_js || testDef.setup_js || "";
    if (setupJs) {
      try {
        window.eval(setupJs);
      } catch (e) {
        results.push({
          name: test.name,
          passed: false,
          result: null,
          error: "Setup JS failed: " + e.message,
        });
        dom.window.close();
        continue;
      }
    }

    // Run the test code
    try {
      // Wrap in an async IIFE so tests can use await
      const wrappedCode = `(async () => { ${test.code} })()`;
      let result = window.eval(wrappedCode);

      // If result is a promise, await it
      if (result && typeof result.then === "function") {
        result = await result;
      }

      results.push({
        name: test.name,
        passed: true,
        result: result,
        error: null,
      });
    } catch (e) {
      results.push({
        name: test.name,
        passed: false,
        result: null,
        error: e.message || String(e),
      });
    }

    dom.window.close();
  }

  // Output JSON with prefix marker so Rust can find it even if there's
  // stray stdout output from the bridge or jsdom.
  process.stdout.write("VICTAURI_RESULTS:" + JSON.stringify(results) + "\n");
}

run().catch((e) => {
  process.stderr.write("Fatal error: " + e.message + "\n");
  process.exit(2);
});
