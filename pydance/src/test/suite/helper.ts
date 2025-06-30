import * as vscode from "vscode";
import * as path from "path";

/**
 * Activates the pydance extension
 */
export async function activate(docUri: vscode.Uri) {
  // The extension is triggered for Python files
  const ext = vscode.extensions.getExtension("ToughType.pydance")!;
  await ext.activate();
  try {
    const doc = await vscode.workspace.openTextDocument(docUri);
    await vscode.window.showTextDocument(doc);
    await sleep(2000); // Wait for language server activation
  } catch (e) {
    console.error(e);
  }
}

async function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

const getDocPath = (p: string) => {
  return path.resolve(__dirname, "../../../src/testFixture", p);
};

export const getDocUri = (p: string) => {
  return vscode.Uri.file(getDocPath(p));
};
