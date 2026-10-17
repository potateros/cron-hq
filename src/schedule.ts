import type { Job, QuietWindow, Schedule } from "./config";

const minutesOf = (t: Date) => t.getHours() * 60 + t.getMinutes();
const pad = (n: number) => String(n).padStart(2, "0");

export function inQuiet(q: QuietWindow | null, at: Date): boolean {
  if (!q) return false;
  const m = minutesOf(at);
  return q.start <= q.end ? m >= q.start && m < q.end : m >= q.start || m < q.end;
}

function quietEndAfter(q: QuietWindow, t: Date): Date {
  const end = new Date(t);
  end.setHours(Math.floor(q.end / 60), q.end % 60, 0, 0);
  if (end <= t) end.setDate(end.getDate() + 1);
  return end;
}

export function nextRun(job: Job, lastStart: Date | null, now: Date): Date | null {
  const s = job.schedule;
  if (s.kind === "manual") return null;
  if (s.kind === "every") {
    let next = lastStart ? new Date(lastStart.getTime() + s.seconds * 1000) : null;
    if (!next || next <= now) next = new Date(now.getTime() + s.seconds * 1000);
    return job.quiet && inQuiet(job.quiet, next) ? quietEndAfter(job.quiet, next) : next;
  }
  const times = [...s.times].sort((a, b) => a.hour * 60 + a.minute - (b.hour * 60 + b.minute));
  for (let day = 0; day < 8; day++) {
    for (const { hour, minute } of times) {
      const c = new Date(now);
      c.setDate(c.getDate() + day);
      c.setHours(hour, minute, 0, 0);
      if (c > now && !inQuiet(job.quiet, c)) return c;
    }
  }
  // Every time is inside quiet hours: the runner will skip them all; report the first anyway.
  const c = new Date(now);
  c.setDate(c.getDate() + 1);
  c.setHours(times[0]!.hour, times[0]!.minute, 0, 0);
  return c;
}

export function formatCountdown(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  return `T–${pad(h)}:${pad(Math.floor((total % 3600) / 60))}:${pad(total % 60)}`;
}

export function describeSchedule(s: Schedule): string {
  if (s.kind === "manual") return "on demand";
  if (s.kind === "at") return `daily ${s.times.map((t) => `${pad(t.hour)}:${pad(t.minute)}`).join(", ")}`;
  if (s.seconds % 3600 === 0) return `every ${s.seconds / 3600}h`;
  if (s.seconds % 60 === 0) return `every ${s.seconds / 60}m`;
  return `every ${s.seconds}s`;
}
