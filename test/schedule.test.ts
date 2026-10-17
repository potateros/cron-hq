import { describe, expect, test } from "bun:test";
import type { Job } from "../src/config";
import { describeSchedule, formatCountdown, inQuiet, nextRun } from "../src/schedule";

const d = (day: number, h: number, m: number) => new Date(2026, 9, day, h, m, 0, 0); // Oct 2026, local time
const NIGHT = { start: 23 * 60, end: 7 * 60 };
const job = (over: Partial<Job>): Job => ({
  id: "j", description: "", command: "true", cwd: "/", quiet: null, timeoutSec: 600, url: null,
  schedule: { kind: "every", seconds: 1800 }, ...over,
});

describe("manual", () => {
  const m = job({ schedule: { kind: "manual" } });
  test("never has a next run", () => expect(nextRun(m, null, d(16, 17, 0))).toBeNull());
  test("described as on demand", () => expect(describeSchedule(m.schedule)).toBe("on demand"));
});

describe("inQuiet", () => {
  test("wrapping window 23:00-07:00", () => {
    for (const [h, m] of [[23, 0], [2, 0], [6, 59]] as const) expect(inQuiet(NIGHT, d(16, h, m))).toBe(true);
    for (const [h, m] of [[7, 0], [12, 0], [22, 59]] as const) expect(inQuiet(NIGHT, d(16, h, m))).toBe(false);
  });
  test("plain window 09:00-17:00", () => {
    const day = { start: 540, end: 1020 };
    expect(inQuiet(day, d(16, 9, 0))).toBe(true);
    expect(inQuiet(day, d(16, 17, 0))).toBe(false);
  });
  test("null window is never quiet", () => expect(inQuiet(null, d(16, 2, 0))).toBe(false));
});

describe("nextRun every", () => {
  const j = job({});
  test("last start + interval", () => expect(nextRun(j, d(16, 17, 30), d(16, 17, 32))).toEqual(d(16, 18, 0)));
  test("never run → now + interval", () => expect(nextRun(j, null, d(16, 17, 32))).toEqual(d(16, 18, 2)));
  test("missed → now + interval", () => expect(nextRun(j, d(16, 15, 0), d(16, 17, 32))).toEqual(d(16, 18, 2)));
  test("lands in quiet → quiet end", () =>
    expect(nextRun(job({ quiet: NIGHT }), d(16, 22, 40), d(16, 22, 45))).toEqual(d(17, 7, 0)));
});

describe("nextRun at", () => {
  test("single time later → tomorrow", () =>
    expect(nextRun(job({ schedule: { kind: "at", times: [{ hour: 6, minute: 55 }] } }), null, d(16, 17, 32))).toEqual(d(17, 6, 55)));
  test("picks the next of several", () =>
    expect(nextRun(job({ schedule: { kind: "at", times: [{ hour: 6, minute: 55 }, { hour: 19, minute: 30 }] } }), null, d(16, 17, 32))).toEqual(d(16, 19, 30)));
  test("skips times inside quiet", () =>
    expect(nextRun(job({ quiet: NIGHT, schedule: { kind: "at", times: [{ hour: 23, minute: 30 }, { hour: 8, minute: 0 }] } }), null, d(16, 17, 32))).toEqual(d(17, 8, 0)));
});

describe("formatCountdown", () => {
  test("hh:mm:ss", () => expect(formatCountdown(1672000)).toBe("T–00:27:52"));
  test("negative clamps", () => expect(formatCountdown(-5)).toBe("T–00:00:00"));
  test("over a day", () => expect(formatCountdown(90061000)).toBe("T–25:01:01"));
});

describe("describeSchedule", () => {
  test("every", () => {
    expect(describeSchedule({ kind: "every", seconds: 1800 })).toBe("every 30m");
    expect(describeSchedule({ kind: "every", seconds: 3600 })).toBe("every 1h");
    expect(describeSchedule({ kind: "every", seconds: 90 })).toBe("every 90s");
  });
  test("at", () =>
    expect(describeSchedule({ kind: "at", times: [{ hour: 6, minute: 55 }, { hour: 19, minute: 30 }] })).toBe("daily 06:55, 19:30"));
});
