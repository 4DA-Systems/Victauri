#!/usr/bin/env node

"use strict";

const path = require("path");
const { spawn } = require("child_process");
const fs = require("fs");

const VERSION = "0.3.0";
const BINARY_NAME = process.platform === "win32" ? "victauri-browser-host.exe" : "victauri-browser-host";
const BINARY_PATH = path.join(__dirname, BINARY_NAME);

function getBinaryPath() {
  if (fs.existsSync(BINARY_PATH)) {
    return BINARY_PATH;
  }
  // Fallback: check if installed globally via cargo
  const globalName = process.platform === "win32" ? "victauri-browser-host.exe" : "victauri-browser-host";
  const envPath = process.env.PATH || "";
  const dirs = envPath.split(path.delimiter);
  for (const dir of dirs) {
    const candidate = path.join(dir, globalName);
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return null;
}

function runBinary(args) {
  const binPath = getBinaryPath();
  if (!binPath) {
    console.error("Error: victauri-browser-host binary not found.");
    console.error("Run 'npm install' or 'npx victauri-browser install' to download it.");
    process.exit(1);
  }

  const child = spawn(binPath, args, {
    stdio: "inherit",
    env: process.env,
  });

  child.on("error", (err) => {
    console.error(`Failed to start victauri-browser-host: ${err.message}`);
    process.exit(1);
  });

  child.on("exit", (code) => {
    process.exit(code || 0);
  });
}

const command = process.argv[2] || "serve";

switch (command) {
  case "install": {
    const extensionId = process.argv[3] || undefined;
    const args = ["install"];
    if (extensionId) {
      args.push(extensionId);
    }
    runBinary(args);
    break;
  }

  case "uninstall": {
    runBinary(["uninstall"]);
    break;
  }

  case "serve": {
    runBinary(["serve"]);
    break;
  }

  case "version":
  case "--version":
  case "-v": {
    console.log(`victauri-browser ${VERSION}`);
    const binPath = getBinaryPath();
    if (binPath) {
      runBinary(["version"]);
    }
    break;
  }

  case "help":
  case "--help":
  case "-h": {
    console.log(`victauri-browser ${VERSION}`);
    console.log("");
    console.log("Native messaging host for the Victauri Chrome/Firefox/Edge extension.");
    console.log("Provides an MCP (Model Context Protocol) bridge for AI agents to interact with any website.");
    console.log("");
    console.log("Usage:");
    console.log("  victauri-browser install [extension-id]   Register native messaging host");
    console.log("  victauri-browser uninstall                Remove native messaging host registration");
    console.log("  victauri-browser serve                    Start the MCP server (default)");
    console.log("  victauri-browser version                  Print version");
    console.log("  victauri-browser help                     Show this help");
    console.log("");
    console.log("Environment variables:");
    console.log("  VICTAURI_BROWSER_PORT         Port for MCP server (default: 7474)");
    console.log("  VICTAURI_BROWSER_AUTH_TOKEN    Bearer token for authentication");
    break;
  }

  default: {
    // Pass through any unknown command to the binary
    runBinary(process.argv.slice(2));
    break;
  }
}
