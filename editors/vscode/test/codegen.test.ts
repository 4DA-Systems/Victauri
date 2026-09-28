// R4-VSC1: "Generate test" must never let page-controlled text escape its Rust literal.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { DomExplorerProvider } from "../src/domExplorerView";
import { escapeRustStr, generateCommandTest } from "../src/rustCodegen";

const INJECTION =
  'x").expect(&mut client).to_be_visible().await.unwrap(); std::process::exit(1); Locator::text("y';

function provider(): DomExplorerProvider {
  const noop = () => ({ dispose: () => undefined });
  const client = { onDidUpdateData: noop, onDidChangeState: noop };
  return new DomExplorerProvider(client as never);
}

/** Lex the Rust string literal starting at `line[start] === '"'`; return the index after it. */
function endOfRustLiteral(line: string, start: number): number {
  assert.equal(line[start], '"');
  for (let i = start + 1; i < line.length; i++) {
    if (line[i] === "\\") {
      i++;
      continue;
    }
    if (line[i] === '"') return i + 1;
  }
  throw new Error(`unterminated literal in: ${line}`);
}

test("an accessible name cannot break out of Locator::text", () => {
  const code = provider().generateTestCode({ tag: "div", ref_id: "e1", name: INJECTION });
  const line = code.split("\n").find((l) => l.includes("Locator::text("));
  assert.ok(line, code);
  const open = line.indexOf("Locator::text(") + "Locator::text(".length;
  const end = endOfRustLiteral(line, open);
  // The whole injected text stays inside the one literal: after it comes only `)`.
  assert.equal(line.slice(end), ")", `the literal must end the call:\n${code}`);
  assert.ok(line.slice(open, end).includes("std::process::exit(1)"), line);
});

test("a ref id cannot break out of client.click / client.fill", () => {
  for (const tag of ["button", "input"]) {
    const code = provider().generateTestCode({ tag, ref_id: INJECTION });
    const line = code.split("\n")[1];
    const open = line.indexOf("(") + 1;
    const end = endOfRustLiteral(line, open);
    assert.ok(/^(\)|, "test value"\))\.await\.unwrap\(\);$/.test(line.slice(end)), line);
  }
});

test("control, bidi and newline characters are escaped like escape_rust_str", () => {
  assert.equal(escapeRustStr('a"b\\c'), 'a\\"b\\\\c');
  assert.equal(escapeRustStr("l1\nl2\r\t\0"), "l1\\nl2\\r\\t\\0");
  assert.equal(escapeRustStr("admin‮txt"), "admin\\u{202e}txt");
  assert.equal(escapeRustStr("esc\u001b[2J"), "esc\\u{1b}[2J");
  assert.equal(escapeRustStr("lone \ud800 surrogate"), "lone \\u{fffd} surrogate");
  assert.equal(escapeRustStr("ünïcödé 😀 {x}"), "ünïcödé 😀 {x}");
});

test("generated command tests are unchanged for normal names", () => {
  const code = generateCommandTest("get_settings");
  assert.ok(code.startsWith("e2e_test!(test_get_settings, |client| async move {"), code);
  assert.ok(code.includes('.invoke_command("get_settings", None)'), code);
});

// The real proof: rustc lexes every generated literal back to the original text.
test("rustc reads each escaped literal back as the original text", (t) => {
  try {
    execFileSync("rustc", ["--version"], { stdio: "ignore" });
  } catch {
    t.skip("rustc not available");
    return;
  }
  const samples = [INJECTION, "a\nb\r\tc\0", "admin‮txt⁦", "esc\u001b[2J", "ok 😀 {}"];
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "victauri-vsc-codegen-"));
  try {
    const src = path.join(dir, "main.rs");
    const lits = samples.map((s) => `"${escapeRustStr(s)}"`).join(", ");
    fs.writeFileSync(
      src,
      `fn main() { for s in [${lits}] { print!("{}\\u{1}", s.escape_default()); } }\n`
    );
    const exe = path.join(dir, process.platform === "win32" ? "main.exe" : "main");
    execFileSync("rustc", ["--edition", "2021", "-o", exe, src], { stdio: "pipe" });
    const got = execFileSync(exe).toString("utf-8").split("\u0001").slice(0, -1);
    // Compare via Rust's own escape_default rendering of the expected strings.
    const expected = samples.map((s) => rustEscapeDefault(s));
    assert.deepEqual(got, expected);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

/** `str::escape_default` in JS, for comparing what rustc decoded. */
function rustEscapeDefault(s: string): string {
  let out = "";
  for (const ch of s) {
    const cp = ch.codePointAt(0)!;
    if (ch === "\t") out += "\\t";
    else if (ch === "\r") out += "\\r";
    else if (ch === "\n") out += "\\n";
    else if (ch === "\\" || ch === "'" || ch === '"') out += "\\" + ch;
    else if (cp >= 0x20 && cp <= 0x7e) out += ch;
    else out += `\\u{${cp.toString(16)}}`;
  }
  return out;
}
