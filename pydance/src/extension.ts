import * as path from "path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  Trace,
} from "vscode-languageclient/node";

let client: LanguageClient;

export function activate(context: vscode.ExtensionContext) {
  const outputChannel = vscode.window.createOutputChannel("Pydance");
  outputChannel.appendLine("Pydance extension is activating...");

  const serverPath = context.asAbsolutePath(path.join("pylight"));
  outputChannel.appendLine(`Server path: ${serverPath}`);

  // Get configuration
  const config = vscode.workspace.getConfiguration("pydance");
  const parser = config.get<string>("parser", "ruff");
  const traceLevel = config.get<string>("trace.server", "off");
  const excludePatterns = config.get<string[]>("excludePatterns", []);
  outputChannel.appendLine(`Using parser: ${parser}`);
  // If the extension is launched in debug mode then the debug server options are used
  const serverOptions: ServerOptions = {
    run: { command: serverPath, args: ["--parser", parser] },
    debug: { command: serverPath, args: ["--parser", parser] },
  };

  // Options to control the language client
  const clientOptions: LanguageClientOptions = {
    // Register the server for Python documents
    documentSelector: [{ scheme: "file", language: "python" }],
    outputChannel: outputChannel,
    // traceOutputChannel: outputChannel,
    initializationOptions: {
      excludePatterns: excludePatterns,
    },
  };

  // Create the language client and start the client.
  client = new LanguageClient(
    "pydance",
    "Pydance",
    // serverOptions,
    serverOptions,
    clientOptions
  );

  // Set trace level based on configuration
  const traceMap: { [key: string]: Trace } = {
    off: Trace.Off,
    messages: Trace.Messages,
    verbose: Trace.Verbose,
  };
  client.setTrace(traceMap[traceLevel] || Trace.Off);

  outputChannel.appendLine("Starting language client...");
  // Start the client. This will also launch the server
  client.start();
  outputChannel.appendLine("Language client started.");
}

export function deactivate(): Promise<void> | undefined {
  if (!client) {
    return undefined;
  }
  return client.stop();
}
