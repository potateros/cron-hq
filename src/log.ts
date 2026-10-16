import { appendFileSync, existsSync, mkdirSync, renameSync, statSync } from "node:fs";
import { dirname } from "node:path";
import { logPath } from "./paths";

export function appendLog(id: string, chunk: string | Uint8Array): void {
  const path = logPath(id);
  mkdirSync(dirname(path), { recursive: true });
  appendFileSync(path, chunk);
}

export function rotateIfNeeded(id: string, maxBytes = 1_048_576): void {
  const path = logPath(id);
  if (existsSync(path) && statSync(path).size > maxBytes) renameSync(path, `${path}.1`);
}

/** Keeps the last N lines of a stream, including a trailing partial line. */
export class TailBuffer {
  private lines: string[] = [];
  private partial = "";

  constructor(private readonly max = 20) {}

  push(chunk: string): void {
    const parts = (this.partial + chunk).split("\n");
    this.partial = parts.pop()!;
    this.lines.push(...parts);
    if (this.lines.length > this.max) this.lines.splice(0, this.lines.length - this.max);
  }

  text(): string {
    const all = this.partial ? [...this.lines, this.partial] : this.lines;
    return all.slice(-this.max).join("\n");
  }
}
