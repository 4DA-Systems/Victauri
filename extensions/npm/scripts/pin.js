"use strict";

// Single source of truth for the release-binary integrity pin (audit #1). Both
// the postinstall (download-time) and the bin launcher (run-time) verify against
// this, so a binary tampered AFTER install is still rejected before execution.

const fs = require("fs");
const crypto = require("crypto");

// Version is derived from package.json so the pin can never drift from the
// published package version (audit #11).
const VERSION = require("../package.json").version;
const BINARY_NAME = "victauri-browser-host";

// Pinned SHA-256 of every release artifact. MUST be regenerated for each release:
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

// Map Node platform/arch -> the published release asset name. Returns null on an
// unsupported platform (the caller decides whether that is fatal).
function getAssetName() {
  const key = `${process.platform}-${process.arch}`;
  const map = {
    "linux-x64": "victauri-browser-host-linux-x86_64",
    "darwin-x64": "victauri-browser-host-macos-x86_64",
    "darwin-arm64": "victauri-browser-host-macos-aarch64",
    "win32-x64": "victauri-browser-host-windows-x86_64.exe",
  };
  return map[key] || null;
}

function expectedHashFor(asset) {
  const perVersion = SHA256[VERSION];
  return perVersion ? perVersion[asset] : undefined;
}

function sha256(buf) {
  return crypto.createHash("sha256").update(buf).digest("hex");
}

// Returns the pinned hash for the current platform's asset, or undefined if the
// platform is unsupported or no pin exists for this version.
function expectedHash() {
  const asset = getAssetName();
  return asset ? expectedHashFor(asset) : undefined;
}

// Verify a file on disk against the pinned hash. Returns { ok, reason }.
function verifyFile(filePath) {
  const expected = expectedHash();
  if (!expected) {
    return { ok: false, reason: `no pinned SHA-256 for v${VERSION} on this platform` };
  }
  let got;
  try {
    got = sha256(fs.readFileSync(filePath));
  } catch (e) {
    return { ok: false, reason: `cannot read ${filePath}: ${e.message}` };
  }
  if (got !== expected) {
    return { ok: false, reason: `SHA-256 mismatch (expected ${expected}, got ${got})` };
  }
  return { ok: true };
}

module.exports = {
  VERSION,
  BINARY_NAME,
  SHA256,
  getAssetName,
  expectedHash,
  expectedHashFor,
  sha256,
  verifyFile,
};
