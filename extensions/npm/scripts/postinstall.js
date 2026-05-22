"use strict";

const https = require("https");
const http = require("http");
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const VERSION = "0.3.0";
const REPO = "runyourempire/victauri";
const BINARY_NAME = "victauri-browser-host";

/**
 * Map Node.js platform/arch to release artifact naming.
 */
function getPlatformTarget() {
  const platform = process.platform;
  const arch = process.arch;

  const targets = {
    "linux-x64": "linux-x86_64",
    "linux-arm64": "linux-aarch64",
    "darwin-x64": "darwin-x86_64",
    "darwin-arm64": "darwin-aarch64",
    "win32-x64": "win32-x86_64",
  };

  const key = `${platform}-${arch}`;
  const target = targets[key];

  if (!target) {
    console.error(`Unsupported platform: ${platform}-${arch}`);
    console.error("Supported platforms: linux-x64, linux-arm64, darwin-x64, darwin-arm64, win32-x64");
    process.exit(1);
  }

  return target;
}

/**
 * Get the download URL for the binary.
 */
function getDownloadUrl(target) {
  const ext = process.platform === "win32" ? ".exe" : "";
  const filename = `${BINARY_NAME}-${target}${ext}`;
  return `https://github.com/${REPO}/releases/download/v${VERSION}/${filename}`;
}

/**
 * Follow redirects and download a file.
 */
function download(url, dest, maxRedirects = 5) {
  return new Promise((resolve, reject) => {
    if (maxRedirects <= 0) {
      return reject(new Error("Too many redirects"));
    }

    const client = url.startsWith("https") ? https : http;

    client.get(url, { headers: { "User-Agent": "victauri-browser-npm" } }, (res) => {
      // Handle redirects (GitHub releases redirect to S3/CDN)
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return download(res.headers.location, dest, maxRedirects - 1)
          .then(resolve)
          .catch(reject);
      }

      if (res.statusCode !== 200) {
        return reject(new Error(`Download failed: HTTP ${res.statusCode} from ${url}`));
      }

      const file = fs.createWriteStream(dest);
      res.pipe(file);

      file.on("finish", () => {
        file.close(resolve);
      });

      file.on("error", (err) => {
        fs.unlink(dest, () => {});
        reject(err);
      });
    }).on("error", (err) => {
      fs.unlink(dest, () => {});
      reject(err);
    });
  });
}

/**
 * Make the binary executable on Unix platforms.
 */
function makeExecutable(filePath) {
  if (process.platform !== "win32") {
    fs.chmodSync(filePath, 0o755);
  }
}

/**
 * Register the native messaging host by running the binary with 'install'.
 */
function registerHost(binaryPath) {
  try {
    const result = execFileSync(binaryPath, ["install"], {
      encoding: "utf8",
      timeout: 30000,
    });
    console.log(result.trim());
  } catch (err) {
    console.warn("Warning: Could not register native messaging host automatically.");
    console.warn(`Run '${binaryPath} install' manually after installation.`);
    if (err.stderr) {
      console.warn(err.stderr);
    }
  }
}

async function main() {
  const target = getPlatformTarget();
  const url = getDownloadUrl(target);
  const ext = process.platform === "win32" ? ".exe" : "";
  const binaryFilename = `${BINARY_NAME}${ext}`;
  const destDir = path.join(__dirname, "..", "bin");
  const destPath = path.join(destDir, binaryFilename);

  // Skip download if binary already exists (e.g. local development)
  if (fs.existsSync(destPath)) {
    console.log(`victauri-browser-host already exists at ${destPath}`);
    registerHost(destPath);
    return;
  }

  // Ensure bin directory exists
  if (!fs.existsSync(destDir)) {
    fs.mkdirSync(destDir, { recursive: true });
  }

  console.log(`Downloading victauri-browser-host for ${target}...`);
  console.log(`  ${url}`);

  try {
    await download(url, destPath);
  } catch (err) {
    console.error(`\nFailed to download binary: ${err.message}`);
    console.error("");
    console.error("Possible solutions:");
    console.error("  1. Check your internet connection");
    console.error(`  2. Download manually from: https://github.com/${REPO}/releases`);
    console.error("  3. Install from source: cargo install victauri-browser");
    console.error("");
    // Don't fail the install — the user can still install the binary manually
    process.exit(0);
  }

  makeExecutable(destPath);
  console.log(`Installed to ${destPath}`);

  // Register native messaging host
  registerHost(destPath);
}

main().catch((err) => {
  console.error(`postinstall error: ${err.message}`);
  // Don't fail npm install if postinstall has issues
  process.exit(0);
});
