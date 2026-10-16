import { beforeEach, describe, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { render } from "ink-testing-library";
import { History } from "../../src/history";
import type { Exec } from "../../src/launchd";
import { logPath } from "../../src/paths";
import { setPaused } from "../../src/state";
import { App } from "../../src/tui/App";
import { tempHome } from "../helpers";

const tick = (ms = 80) => new Promise((r) => setTimeout(r, ms));
const KEY = { down: "\u001B[B", enter: "\r", esc: "\u001B" };
const exec: Exec = async () => ({ code: 113, stdout: "", stderr: "Could not find service" });

let calls: string[][];
let exited: boolean;
beforeEach(() => {
  const dir = tempHome();
  process.env.HQ_CONFIG = join(dir, "jobs.toml");
  writeFileSync(process.env.HQ_CONFIG, '[jobs.alpha]\ncommand = "true"\nevery = "5m"\n\n[jobs.beta]\ncommand = "true"\nevery = "1h"\n');
  mkdirSync(dirname(logPath("alpha")), { recursive: true });
  writeFileSync(logPath("alpha"), "── RUN 0001 · 2026-10-16 17:30:00 · scheduled ──\nhello from alpha\n── END 0001 · NOMINAL · 1s · exit 0 ──\n");
  calls = [];
  exited = false;
});

async function app() {
  const r = render(
    <App deps={{ exec, history: new History(), spawnCli: (a) => calls.push(a), onExit: () => (exited = true) }} />,
  );
  await tick(150);
  return r;
}

describe("App", () => {
  test("arrow keys move the selection", async () => {
    const r = await app();
    expect(r.lastFrame()).toContain("▸  ALPHA");
    r.stdin.write(KEY.down);
    await tick();
    expect(r.lastFrame()).toContain("▸  BETA");
    r.unmount();
  });

  test("enter opens the log view, esc returns", async () => {
    const r = await app();
    r.stdin.write(KEY.enter);
    await tick();
    expect(r.lastFrame()).toContain("── RUN 0001");
    expect(r.lastFrame()).toContain("hello from alpha");
    r.stdin.write(KEY.esc);
    await tick(150);
    expect(r.lastFrame()).toContain("SYSTEMS");
    r.unmount();
  });

  test("R runs the job and follows its log", async () => {
    const r = await app();
    r.stdin.write("r");
    await tick();
    expect(calls).toEqual([["run", "alpha", "--manual"]]);
    expect(r.lastFrame()).toContain("hello from alpha");
    r.unmount();
  });

  test("P holds, or resumes a held job", async () => {
    const r = await app();
    r.stdin.write("p");
    await tick();
    expect(calls.at(-1)).toEqual(["hold", "alpha"]);
    r.unmount();
    setPaused("alpha", true);
    const r2 = await app();
    r2.stdin.write("p");
    await tick();
    expect(calls.at(-1)).toEqual(["resume", "alpha"]);
    r2.unmount();
  });

  test("S shows the plan; Y applies, anything else cancels", async () => {
    const r = await app();
    r.stdin.write("s");
    await tick();
    expect(r.lastFrame()).toContain("+ alpha");
    expect(r.lastFrame()).toContain("+ beta");
    r.stdin.write("n");
    await tick();
    expect(calls).toEqual([]);
    expect(r.lastFrame()).toContain("SYSTEMS");
    r.stdin.write("s");
    await tick();
    r.stdin.write("y");
    await tick();
    expect(calls).toEqual([["sync", "--yes"]]);
    r.unmount();
  });

  test("Q exits", async () => {
    const r = await app();
    r.stdin.write("q");
    await tick();
    expect(exited).toBe(true);
    r.unmount();
  });

  test("a snapshot failure shows a banner instead of killing the TUI", async () => {
    const boom: Exec = async () => {
      throw new Error("launchctl went away");
    };
    const r = render(<App deps={{ exec: boom, history: new History(), spawnCli: () => {} }} />);
    await tick(150);
    expect(r.lastFrame()).toContain("ERROR — launchctl went away");
    r.unmount();
  });

  test("S with an unreadable agents dir shows the error instead of crashing", async () => {
    writeFileSync(process.env.HQ_LAUNCH_AGENTS_DIR!, "not a dir");
    const r = await app();
    r.stdin.write("s");
    await tick();
    expect(r.lastFrame()).toContain("ERROR — ");
    expect(r.lastFrame()).toContain("ENOTDIR");
    r.unmount();
  });
});
