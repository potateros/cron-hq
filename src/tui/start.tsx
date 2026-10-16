import { render } from "ink";
import { join } from "node:path";
import { exec } from "../exec";
import { History } from "../history";
import { repoRoot } from "../paths";
import { App } from "./App";

export async function startTui(): Promise<void> {
  const cli = join(repoRoot(), "src", "cli.ts");
  const spawnCli = (args: string[]) => {
    Bun.spawn([process.execPath, cli, ...args], { stdio: ["ignore", "ignore", "ignore"], env: process.env }).unref();
  };
  const app = render(<App deps={{ exec, history: new History(), spawnCli }} />);
  await app.waitUntilExit();
}
