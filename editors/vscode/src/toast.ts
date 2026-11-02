// The only place that may call VS Code's toast APIs (a test enforces it).
//
// VS Code parses toast text for `[label](command:…)`, `[label](https://…)` and `[label](file:…)`
// and renders them as links it opens with commands allowed; it has no escape syntax. Tool errors
// carry page-controlled text (an element id in "covered by div#…", a thrown message), so without
// this a page could plant a "Retry" link that runs a command in the developer's terminal.
import * as vscode from "vscode";

/** Make `s` unparseable as a VS Code link while leaving it readable. */
export function neutralizeLinks(s: string): string {
  return s.replace(/\]\(/g, "] (");
}

export function showError(text: string): void {
  void vscode.window.showErrorMessage(neutralizeLinks(text));
}

export function showWarning(text: string): void {
  void vscode.window.showWarningMessage(neutralizeLinks(text));
}

export function showInfo(text: string): void {
  void vscode.window.showInformationMessage(neutralizeLinks(text));
}

/** A modal yes/no: true only when the user pressed `button`. */
export async function confirmModal(text: string, button: string): Promise<boolean> {
  const picked = await vscode.window.showWarningMessage(
    neutralizeLinks(text),
    { modal: true },
    button
  );
  return picked === button;
}
