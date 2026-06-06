"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");
// Shared integrity pin — single source of truth for the version, asset map, and
// SHA-256 hashes, used by both this installer and the bin launcher (audit #1).
const pin = require("./pin.js");

const VERSION = pin.VERSION;
const REPO = "runyourempire/victauri";
const BINARY_NAME = pin.BINARY_NAME;
const sha256 = pin.sha256;

// Wraps the shared asset map with a user-facing warning on unsupported platforms.
function getAssetName() {
  const asset = pin.getAssetName();
  if (!asset) {
    console.warn(
      `victauri-browser: no prebuilt binary for ${process.platform}-${process.arch}.`
    );
    console.warn("Build from source: cargo install victauri-browser");
  }
  return asset; // null on unsupported platform — non-fatal
}

function expectedHash(asset) {
  return pin.expectedHashFor(asset);
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

// Whether the user has opted into "optional" installs (`npm install --no-optional`
// or `npm_config_optional=false`). When set, a download outage is treated as
// non-fatal so it can't block an unrelated `npm install`; otherwise we fail loudly.
function optionalInstall() {
  const v = process.env.npm_config_optional;
  return v === "false" || v === "0";
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
    // The download failed, so the binary is NOT installed. Previously this
    // returned success and `npm install` looked clean while the package was
    // unusable (silent failure, audit C5). Fail loudly with a non-zero exit so
    // CI and users see the problem, while still printing recovery steps.
    // Honour the standard opt-out so a download outage can't wedge an unrelated
    // `npm install` for users who accept installing the binary later.
    console.error(`\nvictauri-browser: failed to download ${asset} (v${VERSION}): ${err.message}`);
    console.error(`  Download manually: https://github.com/${REPO}/releases/tag/v${VERSION}`);
    console.error("  Or build from source: cargo install victauri-browser");
    if (optionalInstall()) {
      console.error("  (npm_config_optional set — treating as non-fatal)");
      return;
    }
    process.exit(1);
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
  // An unexpected error means the binary may not be installed. Fail loudly with a
  // non-zero exit (audit C5) instead of masking it as success — unless the user
  // opted into optional installs, in which case stay non-fatal.
  console.error(`victauri-browser postinstall error: ${err.message}`);
  if (optionalInstall()) {
    console.error("  (npm_config_optional set — treating as non-fatal)");
    process.exit(0);
  }
  process.exit(1);
});
