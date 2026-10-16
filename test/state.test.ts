import { beforeEach, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { statePath } from "../src/paths";
import { readPaused, setPaused } from "../src/state";
import { tempHome } from "./helpers";

beforeEach(() => tempHome());

test("setPaused persists across reads", () => {
  expect(readPaused().size).toBe(0);
  setPaused("a", true);
  setPaused("b", true);
  setPaused("a", false);
  expect([...readPaused()]).toEqual(["b"]);
});

test("corrupt state.json → empty set", () => {
  mkdirSync(dirname(statePath()), { recursive: true });
  writeFileSync(statePath(), "{not json");
  expect(readPaused().size).toBe(0);
});
