import type { JobView } from "../snapshot";

export const color = {
  body: "#7f9aa5", // dim cyan-grey
  accent: "#5ee7ff", // bright cyan
  fault: "#ffb347", // amber
  hold: "#5a6670", // grey
} as const;

export const MAX_WIDTH = 90;
/** Left/right margin; everything aligns to the divider between them. */
export const MARGIN = 2;
/** Indent of a job's second line and of detail fields: lines up under the job name. */
export const INDENT = 3;

/** Width of the content area (the divider's length) for a terminal this wide. */
export const innerWidth = (columns: number | undefined) => Math.min(columns || MAX_WIDTH, MAX_WIDTH) - 2 * MARGIN;

/** Word-wrap `text` to `width`, keeping at most `max` lines; an overflowing last line ends with "...". */
export function clampLines(text: string, width: number, max = 2): string[] {
  const lines: string[] = [];
  let cur = "";
  for (let word of text.split(/\s+/).filter(Boolean)) {
    while (word.length > width) {
      const room = cur ? width - cur.length - 1 : width;
      if (room > 0) {
        lines.push(cur ? `${cur} ${word.slice(0, room)}` : word.slice(0, room));
        word = word.slice(room);
      } else lines.push(cur);
      cur = "";
    }
    if (!cur) cur = word;
    else if (cur.length + 1 + word.length <= width) cur += ` ${word}`;
    else {
      lines.push(cur);
      cur = word;
    }
  }
  if (cur) lines.push(cur);
  if (lines.length <= max) return lines;
  const kept = lines.slice(0, max);
  kept[max - 1] = `${kept[max - 1]!.slice(0, width - 3).trimEnd()}...`;
  return kept;
}

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
  return `${time} · ${day} ${now.getDate()} ${mon}`;
}
