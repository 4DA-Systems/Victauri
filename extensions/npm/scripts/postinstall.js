"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const crypto = require("crypto");
const { execFileSync } = require("child_process");

// Version is derived from package.json so the download tag can never drift from
// the published package version (audit finding #11).
const VERSION = require("../package.json").version;
const REPO = "runyourempire/victauri";
const BINARY_NAME = "victauri-browser-host";

// Pinned SHA-256 of every release artifact (audit finding #1). A downloaded binary
// is verified against this map BEFORE it is made executable or run; anything that
// does not match a pinned hash is rejected (fail closed). These MUST be regenerated
// for each release — generate with:
//   gh release download v<VERSION> --pattern 'victauri-browser-host-*'
//   sha256sum victauri-browser-host-*
const SHA256 = {
  "0.7.2": {
    "victauri-browser-host-linux-x86_64":
      "63ceb84bb056e45a88aa89800c94fed69b5cf6666749b69fcb0292a8fdf84904",
    "victauri-browser-host-macos-x86_64":
      "b44f1ac417fb4b708e40e27f7ce14f6049be934a30a982ab6f089a8248d57e6c",
    "victauri-browser-host-macos-aarch64":
      "26d8850e314b181357af4cf6c6041c076ce4ee9722f5295bbd1ab9e6109552f7",
    "victauri-browser-host-windows-x86_64.exe":
      "f91154a026473e59aa0081ddc10ff9f5c81c5fbdce4675f034c435d98be0302e",
  },
};

// Map Node platform/arch -> the actual published release asset name. (The previous
// map produced darwin-*/win32-* names that did not match the published macos-*/
// windows-* assets, so non-Linux installs silently 404'd.)
function getAssetName() {
  const key = `${process.platform}-${process.arch}`;
  const map = {
    "linux-x64": "victauri-browser-host-linux-x86_64",
    "darwin-x64": "victauri-browser-host-macos-x86_64",
    "darwin-arm64": "victauri-browser-host-macos-aarch64",
    "win32-x64": "victauri-browser-host-windows-x86_64.exe",
  };
  const asset = map[key];
  if (!asset) {
    console.warn(`victauri-browser: no prebuilt binary for ${key}.`);
    console.warn("Build from source: cargo install victauri-browser");
    return null; // non-fatal: don't break `npm install`
  }
  return asset;
}

function expectedHash(asset) {
  const perVersion = SHA256[VERSION];
  return perVersion ? perVersion[asset] : undefined;
}

function sha256(buf) {
  return crypto.createHash("sha256").update(buf).digest("hex");
}

// HTTPS-only download into memory. Redirects are followed ONLY to https:// URLs —
// the previous code chose its client by URL prefix, so a 30x to http:// was fetched
// in cleartext (audit #1). Buffering in memory lets us verify the hash before any
// bytes are written to an executable path.
function downloadToBuffer(url, maxRedirects = 5) {
  return new Promise((resolve, reject) => {
    if (maxRedirects <= 0) return reject(new Error("Too many redirects"));
    if (!url.startsWith("https://")) {
      return reject(new Error(`Refusing non-HTTPS URL: ${url}`));
    }
    https
      .get(url, { headers: { "User-Agent": "victauri-browser-npm" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          return downloadToBuffer(res.headers.location, maxRedirects - 1)
            .then(resolve)
            .catch(reject);
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} from ${url}`));
        }
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}

// Register the native messaging host by running the (already hash-verified) binary.
function registerHost(binaryPath) {
  try {
    const result = execFileSync(binaryPath, ["install"], {
      encoding: "utf8",
      timeout: 30000,
    });
    console.log(result.trim());
  } catch (err) {
    console.warn("Warning: could not register native messaging host automatically.");
    console.warn(`Run '${binaryPath} install' manually after installation.`);
    if (err.stderr) console.warn(err.stderr);
  }
}

async function main() {
  const asset = getAssetName();
  if (!asset) return; // unsupported platform, already warned — non-fatal

  const expected = expectedHash(asset);
  if (!expected) {
    // No pinned hash for this version/asset -> we cannot verify it. Fail closed:
    // never download+execute an unverifiable binary.
    console.error(`victauri-browser: no pinned SHA-256 for ${asset} at v${VERSION}.`);
    console.error("Refusing to install an unverifiable binary.");
    console.error("Build from source instead: cargo install victauri-browser");
    process.exit(1);
  }

  const ext = process.platform === "win32" ? ".exe" : "";
  const binaryFilename = `${BINARY_NAME}${ext}`;
  const destDir = path.join(__dirname, "..", "bin");
  const destPath = path.join(destDir, binaryFilename);

  // Local-dev / re-install: if a binary is already present, only trust it if it
  // matches the pinned hash; otherwise re-download.
  if (fs.existsSync(destPath)) {
    if (sha256(fs.readFileSync(destPath)) === expected) {
      console.log(`victauri-browser-host present and verified at ${destPath}`);
      registerHost(destPath);
      return;
    }
    console.warn(`Existing binary at ${destPath} failed hash check — re-downloading.`);
  }

  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${asset}`;
  console.log(`Downloading ${asset} (v${VERSION})...`);

  let buf;
  try {
    buf = await downloadToBuffer(url);
  } catch (err) {
    // Network/transient failure: nothing unsafe happened (no binary written/run),
    // so don't hard-fail `npm install` — guide the user to install manually.
    console.error(`\nFailed to download binary: ${err.message}`);
    console.error(`  Download manually: https://github.com/${REPO}/releases/tag/v${VERSION}`);
    console.error("  Or build from source: cargo install victauri-browser");
    return;
  }

  const got = sha256(buf);
  if (got !== expected) {
    // Integrity failure -> the artifact was tampered or mismatched. Fail closed:
    // do NOT write, chmod, or execute it.
    console.error(`victauri-browser: SHA-256 mismatch for ${asset}`);
    console.error(`  expected ${expected}`);
    console.error(`  got      ${got}`);
    console.error("Refusing to install a binary that does not match the pinned hash.");
    process.exit(1);
  }

  if (!fs.existsSync(destDir)) fs.mkdirSync(destDir, { recursive: true });
  // Write with restrictive-but-executable mode (no-op on Windows).
  fs.writeFileSync(destPath, buf, { mode: 0o755 });
  console.log(`Verified (sha256 ok) and installed to ${destPath}`);

  registerHost(destPath);
}

main().catch((err) => {
  console.error(`postinstall error: ${err.message}`);
  // Unexpected error after the security-critical checks — don't break npm install.
  process.exit(0);
});
