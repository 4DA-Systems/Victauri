// R4-VSC1: "Generate test" must never let page-controlled text escape its Rust literal.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { TauriCommandLensProvider } from "../src/codeLens";
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

test("an accessible name cannot break out of a Locator literal", () => {
  for (const node of [
    { tag: "div", name: INJECTION },
    { tag: "button", role: "button", name: INJECTION },
    { tag: "input", role: "textbox", name: INJECTION },
  ]) {
    const code = provider().generateTestCode({ ...node, ref_id: "e1" });
    const line = code.split("\n").find((l) => l.includes(INJECTION.slice(0, 1) + "\\\""));
    assert.ok(line, code);
    const open = line.indexOf('"', line.indexOf("Locator::") === -1 ? 0 : line.lastIndexOf('("') + 1);
    const end = endOfRustLiteral(line, open);
    assert.ok(line.slice(open, end).includes("std::process::exit(1)"), line);
    assert.ok(/^\)/.test(line.slice(end)), `the literal must end the call:\n${code}`);
  }
});

test("a generated test selects by role and name: a ref means nothing on a fresh app", () => {
  const button = provider().generateTestCode({
    tag: "button",
    role: "tab",
    name: "Home",
    ref_id: "e5",
  });
  assert.ok(button.includes('Locator::role("tab")'), button);
  assert.ok(button.includes('.name("Home")'), button);
  assert.ok(button.includes(".click(&mut client)"), button);
  assert.ok(!button.includes("e5"), `no session-scoped ref: ${button}`);

  const input = provider().generateTestCode({ tag: "input", role: "textbox", name: "Email" });
  assert.ok(input.includes('.fill(&mut client, "test value")'), input);

  const heading = provider().generateTestCode({ tag: "h1", role: "heading", text: "Title" });
  assert.ok(heading.includes('Locator::text("Title")'), heading);
  assert.ok(heading.includes(".to_be_visible()"), heading);
});

test("an element with nothing to locate it by is a visible todo, not an empty passing test", () => {
  const code = provider().generateTestCode({ tag: "div", ref_id: "e9" });
  assert.ok(code.includes("todo!("), code);
  const hostile = provider().generateTestCode({ tag: INJECTION });
  const line = hostile.split("\n").find((l) => l.includes("todo!("));
  assert.ok(line, hostile);
  const open = line.indexOf('"', line.indexOf(", ") + 1);
  assert.equal(line.slice(endOfRustLiteral(line, open)), ");", line);
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
  assert.ok(code.includes("e2e_test!(test_get_settings, |client| async move {"), code);
  assert.ok(code.includes('.invoke_command("get_settings", None)'), code);
});

test("a generated command test does not reject a command that returns null, a bool or a list", () => {
  const code = generateCommandTest("save");
  assert.ok(!code.includes("is_object()"), code);
  assert.ok(code.includes(".unwrap()"), "a command error still fails the test");
});

function lensNames(source: string): string[] {
  const lines = source.split("\n");
  const doc = {
    languageId: "rust",
    lineCount: lines.length,
    lineAt: (i: number) => ({ text: lines[i] }),
    uri: {},
  };
  return new TauriCommandLensProvider()
    .provideCodeLenses(doc as never)
    .map((l) => (l.command?.arguments as string[])[0]);
}

test("CodeLens finds every form of command function", () => {
  for (const [sig, name] of [
    ["fn a()", "a"],
    ["pub fn b()", "b"],
    ["async fn c()", "c"],
    ["pub async fn d()", "d"],
    ["pub(crate) fn e()", "e"],
    ["pub(crate) async fn f()", "f"],
    ["pub(in crate::x) async fn g()", "g"],
    ["async unsafe fn h()", "h"],
    ["pub unsafe fn i()", "i"],
    ["pub fn r#type()", "type"],
  ]) {
    assert.deepEqual(lensNames(`#[tauri::command]\n${sig} {}`), [name], sig);
  }
  assert.deepEqual(lensNames('#[tauri::command(rename_all = "snake_case")]\nfn j() {}'), ["j"]);
  assert.deepEqual(lensNames("#[tauri::command]\n#[inspectable]\nfn k() {}"), ["k"]);
  assert.deepEqual(lensNames("fn not_a_command() {}"), []);
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
