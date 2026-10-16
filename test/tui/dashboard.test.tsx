import { describe, expect, test } from "bun:test";
import { render } from "ink-testing-library";
import { Dashboard } from "../../src/tui/Dashboard";
import { NOW, snapshot } from "../fixtures/snapshot";

const frame = (selected = 0, snap = snapshot) => render(<Dashboard snapshot={snap} selected={selected} now={NOW} />).lastFrame()!;
const rowOf = (f: string, id: string) => f.split("\n").find((l) => l.includes(id))!;

describe("Dashboard", () => {
  test("title and summary", () => {
    const f = frame();
    expect(f).toContain("◢ H Q");
    expect(f).toContain("17:32:08  ·  FRI 16 OCT");
    expect(f).toContain("SYSTEMS  04");
    expect(f).toContain("NOMINAL  02");
    expect(f).toContain("FAULT  01");
    expect(f).toContain("STANDBY  01");
  });

  test("job rows", () => {
    const f = frame();
    expect(rowOf(f, "EINK-WEATHER")).toContain("◉");
    expect(rowOf(f, "EINK-WEATHER")).toContain("T–00:27:52");
    expect(rowOf(f, "TAILSCALE-WATCHDOG")).toContain("◆");
    expect(rowOf(f, "TAILSCALE-WATCHDOG")).toContain("FAULT · EXIT 1");
    expect(rowOf(f, "EINK-WIPE")).toContain("○");
    expect(rowOf(f, "EINK-WIPE")).toContain("HOLD");
    expect(rowOf(f, "TRML-REMINDERS")).toContain("⚠ NOT SYNCED");
  });

  test("second line: schedule, last run, duration and history strip", () => {
    const lines = frame().split("\n");
    const i = lines.findIndex((l) => l.includes("◉  EINK-WEATHER"));
    expect(lines[i + 1]).toContain("every 30m  ·  last 17:30  ·  41s");
    expect(lines[i + 1]).toContain("· █ ▁"); // oldest → newest: skipped, failed, ok
    expect(lines[lines.findIndex((l) => l.includes("EINK-WIPE")) + 1]).toContain("daily 06:55  ·  paused");
  });

  test("detail panel for the selected job", () => {
    const f = frame(0);
    expect(f).toContain("▸  EINK-WEATHER  —  Punggol weather → nRF e-ink");
    expect(f).toContain("cmd       uv run python -m epd.cli weather --push");
    expect(f).toContain("dir       /Users/x/Developer/potateros/nrf5-eink");
    expect(f).toContain("quiet     23:00 – 07:00");
    expect(f).toContain("timeout   5m");
    expect(f).toContain("launchd   linked");
    expect(f).toContain("› pushed (battery 2921 mV, clock drift -3s), fallback armed for midnight");
    expect(frame(1)).toContain("▸  TAILSCALE-WATCHDOG");
  });

  test("footer keys", () => expect(frame()).toContain("↑↓ select     ⏎ logs     R run     P hold     S sync     Q exit"));

  test("config error banner keeps the job list", () => {
    const f = frame(0, { ...snapshot, configError: "jobs.eink: unknown key colour" });
    expect(f).toContain("CONFIG ERROR — jobs.eink: unknown key colour");
    expect(f).toContain("EINK-WEATHER");
  });
});
