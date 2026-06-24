import * as path from "path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  Trace,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let serverOptions: ServerOptions;
let clientOptions: LanguageClientOptions;
let trace: Trace = Trace.Off;

async function restartServer() {
  if (client) {
    await client.stop();
  }
  client = new LanguageClient("pydance", "Pydance", serverOptions, clientOptions);
  client.setTrace(trace);
  await client.start();
}

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
  serverOptions = {
    run: { command: serverPath, args: ["--parser", parser] },
    debug: { command: serverPath, args: ["--parser", parser] },
  };

  // Options to control the language client
  clientOptions = {
    // Register the server for Python documents
    documentSelector: [{ scheme: "file", language: "python" }],
    outputChannel: outputChannel,
    // traceOutputChannel: outputChannel,
    initializationOptions: {
      excludePatterns: excludePatterns,
    },
  };

  // Create the language client and start the client.
  client = new LanguageClient("pydance", "Pydance", serverOptions, clientOptions);

  // Set trace level based on configuration
  const traceMap: { [key: string]: Trace } = {
    off: Trace.Off,
    messages: Trace.Messages,
    verbose: Trace.Verbose,
  };
  trace = traceMap[traceLevel] || Trace.Off;
  client.setTrace(trace);

  context.subscriptions.push(
    vscode.commands.registerCommand("pydance.restartServer", restartServer)
  );

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
