import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { statePath } from "./paths";

export function readPaused(): Set<string> {
  try {
    const data = JSON.parse(readFileSync(statePath(), "utf8")) as { paused?: unknown };
    return new Set(Array.isArray(data.paused) ? data.paused.filter((x) => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

export function setPaused(id: string, paused: boolean): void {
  const set = readPaused();
  if (paused) set.add(id);
  else set.delete(id);
  const path = statePath();
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(`${path}.tmp`, JSON.stringify({ paused: [...set].sort() }, null, 2));
  renameSync(`${path}.tmp`, path);
}
