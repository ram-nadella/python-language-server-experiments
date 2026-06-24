import * as vscode from "vscode";
import * as assert from "assert";
import { getDocUri, activate } from "./helper";
import { registerMockProviders } from "./mockLanguageServer";

suite("Extension Test Suite", () => {
  const docUri = getDocUri("test.py");

  test("Should activate extension", async () => {
    const ext = vscode.extensions.getExtension("ToughType.pydance");
    await ext?.activate();
    assert.ok(ext?.isActive);
  });

  suite("Mock Tests", () => {
    let disposables: vscode.Disposable[] = [];

    setup(() => {
      // Skip mock tests in integration test mode since the real language server will interfere
      if (process.env.INTEGRATION_TEST === "true") {
        return;
      }
      // Register mock providers before each test
      disposables = registerMockProviders();
    });

    teardown(() => {
      // Clean up mock providers after each test
      disposables.forEach((d) => d.dispose());
    });

    test("Should provide workspace symbols with mock provider", async function () {
      // Skip if running in integration test mode
      if (process.env.INTEGRATION_TEST === "true") {
        console.log("Skipping mock test in integration test mode");
        this.skip();
      }
      // Test searching for "test" - should return all symbols containing "test"
      const testSymbols = await vscode.commands.executeCommand<
        vscode.SymbolInformation[]
      >("vscode.executeWorkspaceSymbolProvider", "test");

      assert.ok(Array.isArray(testSymbols));
      assert.strictEqual(testSymbols.length, 5); // All our mock symbols contain "test"

      // Verify specific symbols
      const classSymbol = testSymbols.find((s) => s.name === "TestClass");
      assert.ok(classSymbol);
      assert.strictEqual(classSymbol.kind, vscode.SymbolKind.Class);

      const methodSymbol = testSymbols.find((s) => s.name === "test_method");
      assert.ok(methodSymbol);
      assert.strictEqual(methodSymbol.kind, vscode.SymbolKind.Method);
      assert.strictEqual(methodSymbol.containerName, "TestClass");
    });

    test("Should filter symbols based on query", async function () {
      // Skip if running in integration test mode
      if (process.env.INTEGRATION_TEST === "true") {
        console.log("Skipping mock test in integration test mode");
        this.skip();
      }
      // Test searching for "function"
      const functionSymbols = await vscode.commands.executeCommand<
        vscode.SymbolInformation[]
      >("vscode.executeWorkspaceSymbolProvider", "function");

      assert.strictEqual(functionSymbols.length, 2); // test_function and another_test_function

      const testFunction = functionSymbols.find(
        (s) => s.name === "test_function"
      );
      assert.ok(testFunction);
      assert.strictEqual(testFunction.kind, vscode.SymbolKind.Function);

      const anotherTestFunction = functionSymbols.find(
        (s) => s.name === "another_test_function"
      );
      assert.ok(anotherTestFunction);
      assert.strictEqual(anotherTestFunction.kind, vscode.SymbolKind.Function);
    });
  });

  suite("Integration Tests", () => {
    setup(function () {
      skipUnlessIntegrationReady(this);
    });

    test("Should provide workspace symbols with real language server", async function () {
      await testWorkspaceSymbols(docUri, [
        new vscode.SymbolInformation(
          "TestClass",
          vscode.SymbolKind.Class,
          "",
          new vscode.Location(
            docUri,
            new vscode.Range(
              new vscode.Position(0, 0), // Line 1 (0-indexed)
              new vscode.Position(0, 0)
            )
          )
        ),
        new vscode.SymbolInformation(
          "test_method",
          vscode.SymbolKind.Method, // The server correctly returns Method for methods
          "TestClass",
          new vscode.Location(
            docUri,
            new vscode.Range(
              new vscode.Position(1, 0), // Line 2, column 0 (0-indexed)
              new vscode.Position(1, 0)
            )
          )
        ),
        new vscode.SymbolInformation(
          "test_function",
          vscode.SymbolKind.Function,
          "",
          new vscode.Location(
            docUri,
            new vscode.Range(
              new vscode.Position(5, 0), // Line 6 (0-indexed)
              new vscode.Position(5, 0)
            )
          )
        ),
        new vscode.SymbolInformation(
          "another_test_function",
          vscode.SymbolKind.Function,
          "",
          new vscode.Location(
            docUri,
            new vscode.Range(
              new vscode.Position(9, 0), // Line 10 (0-indexed)
              new vscode.Position(9, 0)
            )
          )
        ),
        new vscode.SymbolInformation(
          "TEST_CONSTANT",
          vscode.SymbolKind.Variable,
          "",
          new vscode.Location(
            docUri,
            new vscode.Range(
              new vscode.Position(13, 0), // Line 14 (0-indexed)
              new vscode.Position(13, 0)
            )
          )
        ),
      ]);
    });

    test("Should index all folders in a multi-root workspace", async function () {
      const repoTwoFolder = secondWorkspaceFolder();
      if (!repoTwoFolder) {
        console.log("No second workspace folder found, skipping multi-root test");
        this.skip();
      }

      await activate(docUri);
      const symbols = await waitForWorkspaceSymbols("repo_two", 2);
      const names = symbols.map((symbol) => symbol.name);

      assert.ok(
        names.includes("repo_two_function"),
        "repo_two_function should be indexed"
      );
      assert.ok(
        names.includes("repo_two_helper"),
        "repo_two_helper should be indexed"
      );
      assert.ok(
        symbols.every((symbol) =>
          symbol.location.uri.fsPath.startsWith(repoTwoFolder!.uri.fsPath)
        ),
        "repo_two results should come from the second workspace folder"
      );
    });

    test("Should expose a restart command that reindexes", async function () {
      await activate(docUri);
      await vscode.commands.executeCommand("pydance.restartServer");

      const symbols = await waitForWorkspaceSymbols("test", 4);
      assert.ok(
        symbols.some((symbol) => symbol.name === "test_function"),
        "Expected test_function after restart"
      );
    });
  });
});

function secondWorkspaceFolder() {
  return vscode.workspace.workspaceFolders?.find(
    (folder) => folder.name === "testFixtureSecond"
  );
}

function skipUnlessIntegrationReady(context: Mocha.Context) {
  if (process.env.INTEGRATION_TEST !== "true") {
    console.log("Not in integration test mode, skipping");
    context.skip();
  }

  const workspaceFolders = vscode.workspace.workspaceFolders;
  if (!workspaceFolders || workspaceFolders.length === 0) {
    console.log("No workspace folder found, skipping integration test");
    context.skip();
  }

  // Check if pylight binary exists
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const fs = require("fs");
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const path = require("path");
  const extensionPath =
    vscode.extensions.getExtension("ToughType.pydance")!.extensionPath;
  const pylightPath = path.join(extensionPath, "pylight");

  if (!fs.existsSync(pylightPath)) {
    console.log("pylight binary not found, skipping integration test");
    context.skip();
  }
}

async function waitForWorkspaceSymbols(query: string, minCount: number) {
  const deadline = Date.now() + 10_000;
  let symbols: vscode.SymbolInformation[] = [];

  while (Date.now() < deadline) {
    symbols = await vscode.commands.executeCommand<vscode.SymbolInformation[]>(
      "vscode.executeWorkspaceSymbolProvider",
      query
    );
    if (symbols.length >= minCount) {
      return symbols;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  assert.fail(
    `Expected at least ${minCount} symbols for ${query}, found ${symbols.length}`
  );
}

async function testWorkspaceSymbols(
  docUri: vscode.Uri,
  expectedSymbols: vscode.SymbolInformation[]
) {
  // Activate extension and language server
  await activate(docUri);

  // Wait a bit more for the language server to index the file
  await new Promise((resolve) => setTimeout(resolve, 1000));

  // Execute workspace symbol search
  const actualSymbols = await vscode.commands.executeCommand<
    vscode.SymbolInformation[]
  >("vscode.executeWorkspaceSymbolProvider", "test");

  // Verify we get an array back
  assert.ok(Array.isArray(actualSymbols));

  // Log what we actually found for debugging
  console.log(
    `Found ${actualSymbols.length} symbols:`,
    actualSymbols.map((s) => ({
      name: s.name,
      kind: s.kind,
      containerName: s.containerName,
      uri: s.location.uri.toString(),
      line: s.location.range.start.line,
      character: s.location.range.start.character,
    }))
  );

  // First check that we got the expected number of symbols
  assert.strictEqual(
    actualSymbols.length,
    expectedSymbols.length,
    `Expected ${expectedSymbols.length} symbols but found ${actualSymbols.length}`
  );

  // Check that we found the expected symbols with correct positions
  for (const expected of expectedSymbols) {
    const found = actualSymbols.find(
      (symbol) => symbol.name === expected.name && symbol.kind === expected.kind
    );
    assert.ok(
      found,
      `Expected to find symbol ${expected.name} of kind ${expected.kind}`
    );

    // Verify the position matches
    const expectedStart = expected.location.range.start;
    const foundStart = found.location.range.start;
    assert.strictEqual(
      foundStart.line,
      expectedStart.line,
      `Symbol ${expected.name} expected at line ${expectedStart.line} but found at line ${foundStart.line}`
    );
  }
}
