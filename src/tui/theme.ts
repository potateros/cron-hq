import type { JobView } from "../snapshot";

export const color = {
  body: "#7f9aa5", // dim cyan-grey
  accent: "#5ee7ff", // bright cyan
  fault: "#ffb347", // amber
  hold: "#5a6670", // grey
} as const;

export const MAX_WIDTH = 90;

export function isFault(v: JobView): boolean {
  return v.last?.status === "failed" || v.last?.status === "timeout";
}

export function glyph(v: JobView): "◉" | "◆" | "○" | "⚠" {
  if (v.held) return "○";
  if (!v.synced) return "⚠";
  return isFault(v) ? "◆" : "◉";
}

export function formatSeconds(sec: number): string {
  if (sec % 3600 === 0) return `${sec / 3600}h`;
  if (sec % 60 === 0) return `${sec / 60}m`;
  return `${sec}s`;
}

const pad = (n: number) => String(n).padStart(2, "0");
export const hhmm = (ms: number) => {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
export const minutesToClock = (m: number) => `${pad(Math.floor(m / 60))}:${pad(m % 60)}`;

export function clockLine(now: Date): string {
  const time = `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`;
  const day = now.toLocaleDateString("en-GB", { weekday: "short" }).toUpperCase();
  const mon = now.toLocaleDateString("en-GB", { month: "short" }).toUpperCase();
  return `${time}  ·  ${day} ${now.getDate()} ${mon}`;
}
