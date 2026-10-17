/** Demo data for tools/screenshots.tsx: the real job set, with made-up runs. */
import type { Job } from "../src/config";
import type { ServiceView, StackView } from "../src/stacks";
import type { RunRow, Status } from "../src/history";
import type { JobView, Snapshot } from "../src/snapshot";

export const NOW = new Date(2026, 9, 16, 21, 32, 8);
const at = (h: number, m: number, s = 0) => new Date(2026, 9, 16, h, m, s).getTime();

let id = 1;
const run = (job: string, status: Status, startedAt: number, secs: number, exitCode: number | null, tail = ""): RunRow => ({
  id: id++, job, trigger: "scheduled", startedAt, endedAt: status === "running" ? null : startedAt + secs * 1000,
  exitCode, status, reason: status === "skipped" ? "quiet" : null, tail,
});
const job = (j: Partial<Job> & Pick<Job, "id" | "command" | "schedule">): Job => ({
  description: "", cwd: "~/Developer/potateros", quiet: null, timeoutSec: 600, url: null, ...j,
});
const linked = { linked: true, running: false, lastExit: 0 };
const every = (from: number, mins: number, n: number, f: (t: number, i: number) => RunRow) =>
  Array.from({ length: n }, (_, i) => f(from - i * mins * 60_000, i));

const weather = every(at(21, 30), 30, 10, (t, i) =>
  i === 6 ? run("eink-weather", "failed", t, 52, 1, "BleakDeviceNotFoundError: NRF_EPD_5E5D not found after 45 s")
    : run("eink-weather", "ok", t, 74 + (i % 3), 0,
        "pushed proverb (battery 2917 mV, clock drift -2s), fallback armed for midnight"));
const wipe = [run("eink-wipe", "ok", at(6, 55), 98, 0, "pushed proverb after CLEAR (battery 2931 mV), fallback armed for midnight")];
const tailscale = [
  run("tailscale-watchdog", "failed", at(21, 16), 31, 1, "2026-10-16 21:16:53  ERROR 'tailscale up' timed out after 30s"),
  ...every(at(20, 16), 60, 6, (t) => run("tailscale-watchdog", "ok", t, 1, 0)),
];
const reminders = [
  run("trml-reminders", "running", at(21, 32, 2), 0, null),
  ...every(at(21, 27), 5, 9, (t) => run("trml-reminders", "ok", t, 5, 0, "117 reminders -> host/data/reminders.json")),
];

const view = (j: Job, recent: RunRow[], next: number | null, over: Partial<JobView> = {}): JobView => ({
  job: j, held: false, agent: linked, synced: true, recent, running: recent[0]?.status === "running",
  last: recent.find((r) => r.status !== "skipped") ?? null, next: next === null ? null : new Date(next), ...over,
});

export const snapshot: Snapshot = {
  at: NOW,
  configError: null,
  jobs: [
    view(job({ id: "eink-weather", description: "Punggol weather → nRF e-ink", cwd: "~/Developer/potateros/nrf5-eink",
      command: "uv run python -m epd.cli --push", schedule: { kind: "every", seconds: 1800 },
      quiet: { start: 1380, end: 420 }, timeoutSec: 300 }), weather, at(22, 0)),
    view(job({ id: "eink-wipe", description: "Daily full clear of the e-ink (ghosting)", cwd: "~/Developer/potateros/nrf5-eink",
      command: "uv run python -m epd.cli --push --wipe", schedule: { kind: "at", times: [{ hour: 6, minute: 55 }] },
      timeoutSec: 300 }), wipe, at(6, 55) + 86_400_000),
    view(job({ id: "tailscale-watchdog", description: "Reconnect Tailscale if it drops", cwd: "~/Developer/potateros/trml-byos",
      command: "host/tailscale-watchdog.sh", schedule: { kind: "every", seconds: 3600 }, timeoutSec: 120 }),
      tailscale, at(22, 16), { agent: { ...linked, lastExit: 1 } }),
    view(job({ id: "trml-reminders", description: "Export macOS Reminders for the TRMNL screen", cwd: "~/Developer/potateros/trml-byos",
      command: "host/reminders-export.sh", schedule: { kind: "every", seconds: 300 }, timeoutSec: 120 }), reminders, at(21, 37)),
  ],
};

export const demoLog = [
  "── RUN 0412 · 2026-10-16 21:00:03 · scheduled ──",
  "scanning for NRF_EPD_5E5D…",
  "connected (fw v1.10-nrf52811, battery 2919 mV)",
  "rendering proverb · uploading 2 planes (rle 3.1 KB)",
  "refresh · holding the link 30 s",
  "pushed proverb (battery 2919 mV, clock drift -2s), fallback armed for midnight",
  "── END 0412 · NOMINAL · 1m15s · exit 0 ──",
  "── RUN 0418 · 2026-10-16 21:30:02 · scheduled ──",
  "scanning for NRF_EPD_5E5D…",
  "connected (fw v1.10-nrf52811, battery 2917 mV)",
  "rendering proverb · uploading 2 planes (rle 3.0 KB)",
  "refresh · holding the link 30 s",
  "pushed proverb (battery 2917 mV, clock drift -2s), fallback armed for midnight",
  "── END 0418 · NOMINAL · 1m14s · exit 0 ──",
  "",
].join("\n");

// ─── services view ──────────────────────────────────────────────────────────

const GiB = 1024 ** 3;
const MiB = 1024 ** 2;
const svc = (stack: string, service: string, image: string, ports: number[], state: ServiceView["state"], cpu: number | null, mem: number | null, status = "Up 6 hours", exitCode = 0): ServiceView => ({
  stack, service, container: state === "missing" ? null : service, image, ports, state, health: "", status, exitCode, cpu, mem,
});
const homelab = [
  svc("homelab", "jellyfin", "jellyfin/jellyfin:latest", [8096], "running", 84.2, 1.9 * GiB),
  svc("homelab", "radarr", "lscr.io/linuxserver/radarr:latest", [7878], "running", 3.1, 412 * MiB),
  svc("homelab", "sonarr", "lscr.io/linuxserver/sonarr:latest", [8989], "running", 2.4, 388 * MiB),
  svc("homelab", "prowlarr", "lscr.io/linuxserver/prowlarr:latest", [9696], "exited", null, null, "Exited (0) 2 hours ago"),
  svc("homelab", "qbittorrent", "lscr.io/linuxserver/qbittorrent:latest", [8080], "exited", null, null, "Exited (0) 2 hours ago"),
  svc("homelab", "tdarr", "haveagitgat/tdarr:latest", [8265], "running", 61.7, 1.2 * GiB),
  svc("homelab", "immich-server", "ghcr.io/immich-app/immich-server:release", [2283], "restarting", 0, 96 * MiB, "Restarting (1) 4 seconds ago", 1),
  svc("homelab", "vaultwarden", "vaultwarden/server:latest", [8222], "running", 0.1, 34 * MiB, "Up 2 days"),
  svc("homelab", "korrosync", "szaffarano/korrosync:latest", [7200], "running", 0, 0.9 * MiB, "Up 39 hours"),
  svc("homelab", "navidrome", "deluan/navidrome:latest", [4533], "missing", null, null, "no container"),
];
const terminus = [
  svc("terminus", "web", "ghcr.io/usetrmnl/terminus:latest", [2300], "running", 0.2, 311 * MiB, "Up 39 hours"),
  svc("terminus", "worker", "ghcr.io/usetrmnl/terminus:latest", [], "running", 0.3, 87 * MiB, "Up 39 hours"),
  svc("terminus", "database", "postgres:17", [5432], "running", 0, 50 * MiB, "Up 39 hours"),
];
export const demoStacks: StackView[] = [
  { stack: { name: "homelab", dir: "~/Developer/potateros/homelab", files: [], envFile: null }, services: homelab, error: null },
  { stack: { name: "terminus", dir: "~/Developer/potateros/trml-byos", files: [], envFile: null }, services: terminus, error: null },
];
