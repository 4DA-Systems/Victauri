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
  "0.7.3": {
    "victauri-browser-host-linux-x86_64":
      "4b3c66ee71ba542670e7a14fb1bc7200b18307cc84534caeceab2bba11b8e75b",
    "victauri-browser-host-macos-x86_64":
      "e7745ab714fcc1ecb6dbcd43912c56615bc50c3b7f5476a1cca4c38dbcbe1599",
    "victauri-browser-host-macos-aarch64":
      "b943394c677d14db579d352277eac4617166742c51579480ac30790868f672e1",
    "victauri-browser-host-windows-x86_64.exe":
      "5dd5ae673f1972dd6fc754da88916562467fb82dd844b95fc682ed7b87b2fd0a",
  },
  "0.7.4": {
    "victauri-browser-host-linux-x86_64":
      "1243584677f48ff7aac30d01d203c0e0de7b547d9d46834c534febf80c0282d7",
    "victauri-browser-host-macos-x86_64":
      "ac5d45b03b1e9f62a1f23207be4c2a9114c0762af2cf98bddb71fcf8c5f1669e",
    "victauri-browser-host-macos-aarch64":
      "ff7a73d74e5f3bedbc29961cbcb1f10acfa0eec529a7dd0ac0ddc6acf4d55fb7",
    "victauri-browser-host-windows-x86_64.exe":
      "9dc44b9fcb4d7b70a80f6911b4b31794c89fd4c22a1392a3da8d12da816afc15",
  },
  "0.7.6": {
    "victauri-browser-host-linux-x86_64":
      "2a8185142eab17013d47d5b5d15711cfdf7b02da094ce7d357718a7ea54d548b",
    "victauri-browser-host-macos-x86_64":
      "d7c9234df1bd29f7c3d5f5fa7bcdab5010f5a1e117029ea198179e7af5613d11",
    "victauri-browser-host-macos-aarch64":
      "d8de4b0f8abfedb31dbf869490fc1cd9a17113dc4b0a9e6392ef0c2cc3a0dc90",
    "victauri-browser-host-windows-x86_64.exe":
      "3377fb29afc1cce0703b02295671596f4d3de12fbdc5716ec2bced91250b0352",
  },
  "0.7.9": {
    "victauri-browser-host-linux-x86_64":
      "795e3d9c41b6aa6ede3868285a6ef9518519fd446007cf3d74f1a93d83251a5a",
    "victauri-browser-host-macos-x86_64":
      "0328b395e3a36bc53c8432c01e9a4d045376ee5ef96f4103f49bc363a8bde30f",
    "victauri-browser-host-macos-aarch64":
      "a8da96aa51e75afb4bc12e96a14cf4aa769d555e0154f1475be16508cc08f62b",
    "victauri-browser-host-windows-x86_64.exe":
      "9fe00634598293bc5cfe6f00295f111eb6da0a555d9adc8967e22266f606e61a",
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
