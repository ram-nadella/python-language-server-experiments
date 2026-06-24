import * as fs from "fs";
import * as path from "path";
import { runTests } from "@vscode/test-electron";

async function main() {
  try {
    const extensionDevelopmentPath = path.resolve(__dirname, "../../");
    const extensionTestsPath = path.resolve(__dirname, "./suite/index");
    const testWorkspace = path.resolve(
      __dirname,
      "../../src/testWorkspaces/multi-root.code-workspace"
    );
    const userDataDir = path.join("/tmp", `pydance-vscode-test-${process.pid}`);
    fs.rmSync(userDataDir, { recursive: true, force: true });

    console.log("Running multi-root integration tests with workspace:", testWorkspace);

    await runTests({
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: [testWorkspace, "--user-data-dir", userDataDir],
      extensionTestsEnv: {
        INTEGRATION_TEST: "true",
      },
    });
  } catch (err) {
    console.error("Failed to run multi-root integration tests");
    process.exit(1);
  }
}

main();
