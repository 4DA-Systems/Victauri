import * as vscode from "vscode";

const COMMAND_PATTERN =
  /^\s*#\[tauri::command\]\s*$/;
const FN_PATTERN =
  /^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/;

export class TauriCommandLensProvider implements vscode.CodeLensProvider {
  provideCodeLenses(document: vscode.TextDocument): vscode.CodeLens[] {
    if (document.languageId !== "rust") return [];

    const lenses: vscode.CodeLens[] = [];
    const lineCount = document.lineCount;

    for (let i = 0; i < lineCount; i++) {
      const line = document.lineAt(i).text;
      if (!COMMAND_PATTERN.test(line)) continue;

      // Look ahead for the function name
      for (let j = i + 1; j < Math.min(i + 5, lineCount); j++) {
        const fnLine = document.lineAt(j).text;
        const match = FN_PATTERN.exec(fnLine);
        if (match) {
          const fnName = match[1];
          const range = new vscode.Range(j, 0, j, fnLine.length);

          lenses.push(
            new vscode.CodeLens(range, {
              title: "$(beaker) Generate Victauri test",
              command: "victauri.generateTestForCommand",
              arguments: [fnName, document.uri],
            })
          );
          break;
        }
      }
    }
    return lenses;
  }
}

export function generateCommandTest(fnName: string): string {
  return `e2e_test!(test_${fnName}, |client| async move {
    let result = client
        .invoke_command("${fnName}", None)
        .await
        .unwrap();

    // Verify the command executed successfully
    assert!(result.is_object() || result.is_string() || result.is_number(),
        "${fnName} should return a value, got: {result}");
});
`;
}
