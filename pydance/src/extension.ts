import * as path from 'path';
import * as vscode from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    Trace,
} from 'vscode-languageclient/node';

let client: LanguageClient;

export function activate(context: vscode.ExtensionContext) {
    // Create output channel
    const outputChannel = vscode.window.createOutputChannel("Pydance");
    outputChannel.show();
    outputChannel.appendLine("Pydance extension is activating...");

    const serverPath = context.asAbsolutePath(
        path.join('server', 'target', 'release', 'symbol_search_lsp')
    );
    outputChannel.appendLine(`Server path: ${serverPath}`);

    // Get the workspace root path
    let workspaceRoot = vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders.length > 0
        ? vscode.workspace.workspaceFolders[0].uri.fsPath
        : undefined;

    if (!workspaceRoot) {
        outputChannel.appendLine("No workspace folder found. Server not starting.");
        return;
    }

    outputChannel.appendLine(`Workspace root: ${workspaceRoot}`);

    // If the extension is launched in debug mode then the debug server options are used
    const serverOptions: ServerOptions = {
        run: { command: serverPath, args: ["--directory", workspaceRoot] },
        debug: { command: serverPath, args: ["--directory", workspaceRoot] }
    };

    // Options to control the language client
    const clientOptions: LanguageClientOptions = {
        // Register the server for Python documents
        documentSelector: [{ scheme: 'file', language: 'python' }],
        synchronize: {
            // Notify the server about file changes to Python files in the workspace
            fileEvents: vscode.workspace.createFileSystemWatcher('**/*.py', false, true, true) // Ignore changes in .venv
        },
        outputChannel: outputChannel,
        // traceOutputChannel: outputChannel,
        initializationOptions: {
            excludePatterns: [
                "**/.venv/**",
                "**/venv/**",
                "**/.env/**",
                "**/env/**",
            ]
        }
    };

    // Create the language client and start the client.
    client = new LanguageClient(
        'pydance',
        'Pydance',
        // serverOptions,
        serverOptions,
        clientOptions
    );

    // verbose logging of the LSP client's interactions with the server
    // turn off when packaging the extension (make it configurable)
    client.setTrace(Trace.Verbose);

    outputChannel.appendLine("Starting language client...");
    // Start the client. This will also launch the server
    client.start();
    outputChannel.appendLine("Language client started.");

    // Register the workspace symbol provider command
    const disposable = vscode.commands.registerCommand('pydance.search', async () => {
        const result = await vscode.commands.executeCommand('workbench.action.showAllSymbols');
        outputChannel.appendLine("Symbol search command executed");
    });

    context.subscriptions.push(disposable);
}

export function deactivate(): Promise<void> | undefined {
    if (!client) {
        return undefined;
    }
    return client.stop();
}