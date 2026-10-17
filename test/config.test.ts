import { describe, expect, test } from "bun:test";
import { homedir } from "node:os";
import { ConfigError, parseConfig } from "../src/config";

const SPEC_EXAMPLE = `
[jobs.eink-weather]
description = "Punggol weather → nRF e-ink"
cwd         = "~/Developer/potateros/nrf5-eink"
command     = "uv run python -m epd.cli weather --push"
every       = "30m"
quiet       = "23:00-07:00"
timeout     = "5m"

[jobs.eink-wipe]
description = "Daily full clear of the e-ink (ghosting)"
cwd         = "~/Developer/potateros/nrf5-eink"
command     = "uv run python -m epd.cli weather --push --wipe"
at          = ["06:55"]
timeout     = "5m"

[jobs.trml-reminders]
description = "Apple Reminders → TRMNL sidecar"
command     = "~/Developer/potateros/trml-byos/host/reminders-export.sh"
every       = "5m"

[jobs.tailscale-watchdog]
description = "Restart Tailscale if it's down"
command     = "~/Developer/potateros/trml-byos/host/tailscale-watchdog.sh"
every       = "1h"
`;

const one = (body: string, id = "j") => `[jobs.${id}]\ncommand = "true"\n${body}\n`;

function expectConfigError(text: string, ...mentions: string[]) {
  let err: unknown;
  try {
    parseConfig(text);
  } catch (e) {
    err = e;
  }
  expect(err).toBeInstanceOf(ConfigError);
  for (const m of mentions) expect((err as Error).message).toContain(m);
}

describe("parseConfig", () => {
  test("parses every job", () => {
    const jobs = parseConfig(SPEC_EXAMPLE);
    expect(jobs.map((j) => j.id)).toEqual(["eink-weather", "eink-wipe", "tailscale-watchdog", "trml-reminders"]);
    const w = jobs.find((j) => j.id === "eink-weather")!;
    expect(w.schedule).toEqual({ kind: "every", seconds: 1800 });
    expect(w.quiet).toEqual({ start: 1380, end: 420 });
    expect(w.timeoutSec).toBe(300);
    expect(w.cwd).toBe(`${homedir()}/Developer/potateros/nrf5-eink`);
    expect(jobs.find((j) => j.id === "eink-wipe")!.schedule).toEqual({ kind: "at", times: [{ hour: 6, minute: 55 }] });
    const r = jobs.find((j) => j.id === "trml-reminders")!;
    expect(r.timeoutSec).toBe(600);
    expect(r.cwd).toBe(homedir());
    expect(r.quiet).toBeNull();
  });

  test("rejects both every and at", () => expectConfigError(one('every = "5m"\nat = ["07:00"]', "both"), "both"));
  test("rejects neither every nor at", () => expectConfigError(one("", "neither"), "neither"));
  test("rejects every under 60s", () => expectConfigError(one('every = "30s"'), "every"));
  test("rejects bad duration", () => expectConfigError(one('every = "5x"'), "5x"));
  test("rejects bad quiet", () => expectConfigError(one('every = "5m"\nquiet = "7am-9am"'), "quiet"));
  test("rejects unknown key", () => expectConfigError(one('every = "5m"\ncolour = "red"'), "colour"));
  test("rejects bad id", () => expectConfigError('[jobs.Bad_Id]\ncommand = "true"\nevery = "5m"\n', "Bad_Id"));
  test("rejects missing command", () => expectConfigError('[jobs.nocmd]\nevery = "5m"\n', "nocmd", "command"));
  test("rejects bad at time", () => expectConfigError(one('at = ["25:00"]'), "25:00"));
  test("invalid TOML → ConfigError", () => expectConfigError("[jobs.x\ncommand = ", "TOML"));

  test("manual job: no schedule, sorted first", () => {
    const jobs = parseConfig(one('every = "5m"', "a-sched") + one("manual = true", "z-tool"));
    expect(jobs.map((j) => j.id)).toEqual(["z-tool", "a-sched"]);
    expect(jobs[0]!.schedule).toEqual({ kind: "manual" });
  });
  test("manual = false still needs a schedule", () => expectConfigError(one("manual = false"), "neither"));
  test("rejects manual with every", () => expectConfigError(one('manual = true\nevery = "5m"'), "manual"));
  test("rejects manual with quiet", () => expectConfigError(one('manual = true\nquiet = "23:00-07:00"'), "quiet"));
  test("rejects non-boolean manual", () => expectConfigError(one('manual = "yes"'), "manual"));
});
