import { beforeEach, describe, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { render } from "ink-testing-library";
import { History } from "../../src/history";
import type { Exec } from "../../src/launchd";
import { logPath } from "../../src/paths";
import { App } from "../../src/tui/App";
import { chunk, rowsFor, viewTop } from "../../src/tui/LogView";
import { tempHome } from "../helpers";

describe("chunk / rowsFor", () => {
  test("short lines are one row", () => {
    expect(chunk("abc", 5)).toEqual(["abc"]);
    expect(rowsFor("abc", 5, false)).toEqual(["abc"]);
  });

  test("long lines wrap when expanded and are cut with … otherwise", () => {
    expect(chunk("abcdefghij", 4)).toEqual(["abcd", "efgh", "ij"]);
    expect(rowsFor("abcdefghij", 4, true)).toEqual(["abcd", "efgh", "ij"]);
    expect(rowsFor("abcdefghij", 4, false)).toEqual(["abc…"]);
  });

  test("counts characters, not UTF-16 units", () => {
    expect(chunk("──────", 3)).toEqual(["───", "───"]);
  });
});

describe("viewTop", () => {
  test("bottom anchor packs lines above the cursor", () => {
    expect(viewTop([1, 1, 1, 1, 1], 4, 3, 0, "bottom")).toBe(2);
    expect(viewTop([1, 1, 3, 1], 3, 4, 0, "bottom")).toBe(2);
  });

  test("keep scrolls only as far as needed to show the cursor", () => {
    expect(viewTop([1, 1, 1, 1, 1], 1, 3, 0, "keep")).toBe(0);
    expect(viewTop([1, 1, 1, 1, 1], 3, 3, 0, "keep")).toBe(1);
    expect(viewTop([1, 1, 1, 1, 1], 1, 3, 2, "keep")).toBe(1);
  });

  test("an expanded cursor line pushes earlier lines off the top", () => {
    expect(viewTop([1, 1, 3], 2, 4, 0, "keep")).toBe(1);
  });

  test("a cursor line taller than the screen stays at the top", () => {
    expect(viewTop([1, 9], 1, 4, 0, "keep")).toBe(1);
  });
});

const tick = (ms = 80) => new Promise((r) => setTimeout(r, ms));
const KEY = { up: "\u001B[A", down: "\u001B[B", enter: "\r" };
const exec: Exec = async () => ({ code: 113, stdout: "", stderr: "Could not find service" });
const LONG = `pushed at-a-glance (battery 3322 mV, signal -55 dBm, clock drift -4s), fallback armed for midnight ${"x".repeat(80)} TAIL-MARKER`;

describe("LogView", () => {
  beforeEach(() => {
    const dir = tempHome();
    process.env.HQ_CONFIG = join(dir, "jobs.toml");
    writeFileSync(process.env.HQ_CONFIG, '[jobs.alpha]\ncommand = "true"\nevery = "5m"\n');
    mkdirSync(dirname(logPath("alpha")), { recursive: true });
    writeFileSync(logPath("alpha"), `── RUN 0001 · 2026-10-17 17:28:48 · scheduled ──\n${LONG}\n── END 0001 · NOMINAL · 54s · exit 0 ──\n`);
  });

  async function openLog() {
    const r = render(<App deps={{ exec, history: new History(), spawnCli: () => {}, onExit: () => {} }} />);
    await tick(150);
    r.stdin.write(KEY.enter);
    await tick();
    return r;
  }

  test("opens following the tail with the cursor on the last line", async () => {
    const r = await openLog();
    const frame = r.lastFrame()!;
    expect(frame).toContain("LIVE");
    expect(frame).toContain("▸ ── END 0001");
    expect(frame).not.toContain("TAIL-MARKER");
    expect(frame).toContain("…");
    r.unmount();
  });

  test("⏎ on a selected line expands it, and again collapses it", async () => {
    const r = await openLog();
    r.stdin.write(KEY.up);
    await tick();
    expect(r.lastFrame()).toContain("2/3");
    expect(r.lastFrame()).toContain("▸ pushed at-a-glance");
    r.stdin.write(KEY.enter);
    await tick();
    expect(r.lastFrame()).toContain("TAIL-MARKER");
    r.stdin.write(KEY.enter);
    await tick();
    expect(r.lastFrame()).not.toContain("TAIL-MARKER");
    r.unmount();
  });

  test("w wraps every line; ↓ past the end returns to LIVE", async () => {
    const r = await openLog();
    r.stdin.write("w");
    await tick();
    expect(r.lastFrame()).toContain("TAIL-MARKER");
    expect(r.lastFrame()).toContain("LIVE · WRAP");
    r.stdin.write(KEY.up);
    await tick();
    expect(r.lastFrame()).toContain("2/3 · WRAP");
    r.stdin.write(KEY.down);
    await tick();
    expect(r.lastFrame()).toContain("LIVE · WRAP");
    r.unmount();
  });
});
