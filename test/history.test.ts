import { beforeEach, describe, expect, test } from "bun:test";
import { History } from "../src/history";
import { dbPath } from "../src/paths";
import { tempHome } from "./helpers";

let h: History;
beforeEach(() => {
  tempHome();
  h = new History();
});

describe("History", () => {
  test("start/finish round-trip", () => {
    const id = h.start("a", "manual", 1000);
    expect(h.recent("a", 5)[0]!.status).toBe("running");
    h.finish(id, { endedAt: 2500, exitCode: 0, status: "ok", tail: "hi" });
    expect(h.recent("a", 5)[0]).toEqual({
      id, job: "a", trigger: "manual", startedAt: 1000, endedAt: 2500, exitCode: 0, status: "ok", reason: null, tail: "hi",
    });
  });

  test("skip records reason", () => {
    h.skip("a", "scheduled", "quiet", 5000);
    const r = h.recent("a", 1)[0]!;
    expect([r.status, r.reason, r.startedAt, r.endedAt]).toEqual(["skipped", "quiet", 5000, 5000]);
  });

  test("recent orders newest first and limits", () => {
    for (const t of [1, 3, 2, 4]) h.skip("a", "scheduled", "quiet", t);
    h.skip("b", "scheduled", "quiet", 9);
    expect(h.recent("a", 3).map((r) => r.startedAt)).toEqual([4, 3, 2]);
  });

  test("lastReal ignores skipped", () => {
    const id = h.start("a", "scheduled", 10);
    h.finish(id, { endedAt: 11, exitCode: 1, status: "failed", tail: "" });
    h.skip("a", "scheduled", "overlap", 20);
    expect(h.lastReal("a")!.status).toBe("failed");
    expect(h.lastReal("nope")).toBeNull();
  });

  test("prune deletes only rows older than 90 days", () => {
    const now = 200 * 86400_000;
    h.skip("a", "scheduled", "quiet", now - 91 * 86400_000);
    h.skip("a", "scheduled", "quiet", now - 89 * 86400_000);
    expect(h.prune(now)).toBe(1);
    expect(h.recent("a", 10)).toHaveLength(1);
  });

  test("concurrent reader and writer do not hit SQLITE_BUSY", () => {
    const reader = new History(dbPath());
    expect(() => {
      for (let i = 0; i < 200; i++) {
        const id = h.start("c", "scheduled", i);
        reader.recent("c", 10);
        h.finish(id, { endedAt: i + 1, exitCode: 0, status: "ok", tail: "" });
        reader.lastReal("c");
      }
    }).not.toThrow();
    expect(reader.recent("c", 500)).toHaveLength(200);
  });
});
