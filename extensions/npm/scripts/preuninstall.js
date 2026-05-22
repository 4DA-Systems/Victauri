"use strict";

const path = require("path");
const fs = require("fs");
const { execFileSync } = require("child_process");

const BINARY_NAME = process.platform === "win32" ? "victauri-browser-host.exe" : "victauri-browser-host";
const BINARY_PATH = path.join(__dirname, "..", "bin", BINARY_NAME);

function main() {
  if (!fs.existsSync(BINARY_PATH)) {
    // Binary not present — nothing to unregister
    return;
  }

  try {
    const result = execFileSync(BINARY_PATH, ["uninstall"], {
      encoding: "utf8",
      timeout: 30000,
    });
    if (result.trim()) {
      console.log(result.trim());
    }
  } catch (err) {
    // Best-effort: don't fail uninstall if the binary can't run
    console.warn("Warning: Could not unregister native messaging host.");
    if (err.stderr) {
      console.warn(err.stderr);
    }
  }
}

main();
