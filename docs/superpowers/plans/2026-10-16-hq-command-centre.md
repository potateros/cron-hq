# hq Command Centre Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `hq`, a registry + launchd installer + sci-fi Ink TUI for the owner's small scheduled jobs on the Mac mini.

**Architecture:**
- `jobs.toml` defines jobs, and `hq sync` writes one `com.chngu.hq.<id>` LaunchAgent per job.
- Every LaunchAgent runs `hq run <id>`, which applies hold/quiet/overlap rules, runs the command under `zsh -lc`, writes the job log, and records the run in SQLite.
- The Ink TUI only reads (history DB, state file, `launchctl print`) and shells out to the same CLI for actions.

**Tech Stack:** Bun 1.3 + TypeScript, `bun:sqlite`, `Bun.TOML.parse`, `bun test`, ink 8.0.0, react 19.3.1, ink-testing-library 4.0.0.

**Spec:** `docs/superpowers/specs/2026-10-16-hq-command-centre-design.md`

## Global Constraints

- Repo `~/Developer/potateros/hq`, branch `main`, command name `hq` via `bun link`.
- Config: `$HQ_CONFIG` or `<repo>/jobs.toml`. Data dir: `$HQ_HOME` or `~/Library/Application Support/hq`. LaunchAgents dir: `$HQ_LAUNCH_AGENTS_DIR` or `~/Library/LaunchAgents`. **No test may touch the real dirs or real `launchctl`.**
- Label prefix `com.chngu.hq.`; never create, modify or remove any other label except via `uninstall` restoring `migrated/` backups.
- Job id regex `^[a-z0-9-]+$`. Keys allowed: `description, cwd, command, every, at, quiet, timeout`. Exactly one of `every`/`at`. `every` ≥ 60 s. `timeout` default `10m`. `cwd` default `~`.
- Job command runs as `/bin/zsh -lc <command>` in `cwd`.
- Log header `── RUN 0042 · 2026-10-16 17:30:00 · scheduled ──`; footer `── END 0042 · NOMINAL · 41s · exit 0 ──` (NOMINAL | FAULT | TIMEOUT). Rotate at 1 MB, keep one `.1`.
- Timeout: SIGTERM, then SIGKILL after 10 s. History pruned at 90 days. Tail = last 20 lines.
- TUI copy: title `◢ H Q`; summary `SYSTEMS / NOMINAL / FAULT / STANDBY`; glyphs `◉` nominal, `◆` fault, `○` hold, `⚠ NOT SYNCED`; countdown `T–hh:mm:ss`; keys `↑↓ select  ⏎ logs  R run  P hold  S sync  Q exit`. Palette: body dim cyan-grey (`#7f9aa5`), accent bright cyan (`#5ee7ff`), fault amber (`#ffb347`), hold grey (`#5a6670`).

## Review Focus

1. **`jobs.toml` broken while the TUI is open:** the TUI shows a `CONFIG ERROR — <message>` banner and keeps the last good job list instead of crashing → test in Task 7.
2. **Command not found or `cwd` doesn't exist:** the run is recorded `failed` with the reason in the log/tail; the runner itself never throws → test in Task 4.
3. **TUI reading `history.db` while a runner writes:** no `SQLITE_BUSY` crash (WAL + `busy_timeout = 5000`) → test in Task 3.
4. **Output without a trailing newline, or a burst bigger than the tail:** the tail holds exactly the last 20 lines including the partial last line; the log gets every byte → test in Task 4.
5. **Job never bootstrapped / launchctl reports "Could not find service":** state is `{linked:false}`, shown as `⚠ NOT SYNCED`, never an exception → test in Task 5.

---

### Task 1: Scaffold, paths and config parsing

**Files:**
- Create: `package.json`, `tsconfig.json`, `.gitignore` (`node_modules/`, `migrated/`), `src/paths.ts`, `src/config.ts`, `test/config.test.ts`

**Interfaces:**
- Produces:
  - `paths.ts`:
    - `hqHome(): string`, `dbPath(): string`, `statePath(): string`
    - `logPath(id: string): string`, `lockPath(id: string): string`
    - `launchAgentsDir(): string`, `repoRoot(): string`, `configPath(): string` (= `$HQ_CONFIG` or `<repoRoot>/jobs.toml`)
    - `migratedDir(): string` (= `<repoRoot>/migrated`)
    - `expandHome(p: string): string`
  - `config.ts`:
    - `interface QuietWindow { start: number; end: number }` (minutes after midnight)
    - `type Schedule = { kind: "every"; seconds: number } | { kind: "at"; times: { hour: number; minute: number }[] }`
    - `interface Job { id: string; description: string; command: string; cwd: string; schedule: Schedule; quiet: QuietWindow | null; timeoutSec: number }`
    - `class ConfigError extends Error`
    - `parseConfig(text: string): Job[]` (sorted by id)
    - `loadConfig(path = configPath()): Job[]`
    - `parseDuration(s: string): number` (seconds; `/^(\d+)(s|m|h)$/`)
    - `parseQuiet(s: string): QuietWindow` (`HH:MM-HH:MM`)

- [ ] **Step 1:** `bun init -y`. Then:
  - `bun add ink@8.0.0 react@19.3.1`
  - `bun add -d ink-testing-library@4.0.0 @types/react@19.3.0 @types/bun`
  - `package.json`: `"bin": {"hq": "src/cli.ts"}`. `tsconfig`: `"jsx": "react-jsx"`.
- [ ] **Step 2:** Write the failing tests in `test/config.test.ts`:
  - `parses every job` — the spec §4 example TOML → 4 jobs. `eink-weather`: `schedule {kind:"every",seconds:1800}`, `quiet {start:1380,end:420}`, `timeoutSec 300`, `cwd` = expanded home path. `eink-wipe`: `{kind:"at",times:[{hour:6,minute:55}]}`. `trml-reminders`: `timeoutSec 600`, cwd = `$HOME`.
  - `rejects both every and at` / `rejects neither` → `ConfigError` whose message contains the job id.
  - `rejects every under 60s` (`every="30s"`), `rejects bad duration` (`"5x"`), `rejects bad quiet` (`"7am-9am"`), `rejects unknown key` (`colour="red"`; message names `colour`), `rejects bad id` (`[jobs.Bad_Id]`), `rejects missing command`.
  - `invalid TOML → ConfigError` (not a raw parser error).
- [ ] **Step 3:** Run `bun test test/config.test.ts`. Expected: FAIL (module not found).
- [ ] **Step 4:** Implement `paths.ts` and `config.ts` with the signatures above. Use `Bun.TOML.parse`, wrapping its error in `ConfigError`.
- [ ] **Step 5:** Run `bun test`. Expected: all pass. Also verify Ink runs under Bun: `bun -e 'import {render,Text} from "ink"; import React from "react"; const r=render(React.createElement(Text,null,"ok")); r.unmount()'` prints `ok`.
- [ ] **Step 6:** Commit: `feat: scaffold, paths and jobs.toml parsing`

### Task 2: Schedule maths

**Files:**
- Create: `src/schedule.ts`, `test/schedule.test.ts`

**Interfaces:**
- Consumes: `Job`, `QuietWindow` (Task 1).
- Produces:
  - `inQuiet(q: QuietWindow | null, at: Date): boolean` (start inclusive, end exclusive, wraps midnight)
  - `nextRun(job: Job, lastStart: Date | null, now: Date): Date`
  - `formatCountdown(ms: number): string` → `"T–00:27:52"` (clamps negatives to `T–00:00:00`)
  - `describeSchedule(s: Schedule): string` → `"every 30m"`, `"every 1h"`, `"daily 06:55"`, `"daily 06:55, 19:30"`

- [ ] **Step 1:** Failing tests:
  - Quiet `23:00-07:00`: `inQuiet` is true at 23:00, 02:00, 06:59 and false at 07:00, 12:00, 22:59. Non-wrapping `09:00-17:00`: true at 09:00, false at 17:00.
  - `nextRun every`: 30m job, lastStart 17:30, now 17:32 → 18:00. With lastStart null → now + 30 m. With lastStart 15:00, now 17:32 (missed) → now + 30 m. With quiet 23:00–07:00 and the computed time 23:10 → 07:00 next day.
  - `nextRun at`: `["06:55"]`, now 17:32 → tomorrow 06:55. `["06:55","19:30"]`, now 17:32 → today 19:30. An `at` time inside quiet is skipped to the next non-quiet occurrence.
  - `formatCountdown(1672000)` → `"T–00:27:52"`; `formatCountdown(-5)` → `"T–00:00:00"`; `formatCountdown(90061000)` → `"T–25:01:01"`.
- [ ] **Step 2:** Run `bun test test/schedule.test.ts`. Expected: FAIL.
- [ ] **Step 3:** Implement. All computations use local time (`Date` setters), not UTC.
- [ ] **Step 4:** Run `bun test`. Expected: PASS.
- [ ] **Step 5:** Commit: `feat: schedule maths (quiet windows, next run, countdown)`

### Task 3: History DB and hold state

**Files:**
- Create: `src/history.ts`, `src/state.ts`, `test/history.test.ts`, `test/state.test.ts`

**Interfaces:**
- Produces:
  - `type Status = "running" | "ok" | "failed" | "timeout" | "skipped"`
  - `type Trigger = "scheduled" | "manual"`
  - `interface RunRow { id: number; job: string; trigger: Trigger; startedAt: number; endedAt: number | null; exitCode: number | null; status: Status; reason: string | null; tail: string | null }`
  - `class History`:
    - `constructor(path = dbPath())` — creates the table and index from spec §5, sets `PRAGMA journal_mode=WAL` and `busy_timeout=5000`
    - `start(job: string, trigger: Trigger, startedAt: number): number`
    - `finish(id: number, r: { endedAt: number; exitCode: number | null; status: Exclude<Status, "running" | "skipped">; tail: string }): void`
    - `skip(job: string, trigger: Trigger, reason: "paused" | "quiet" | "overlap", at: number): number`
    - `recent(job: string, limit: number): RunRow[]` (newest first)
    - `lastReal(job: string): RunRow | null` (newest non-skipped)
    - `prune(now: number, maxAgeMs = 90 * 86400_000): number`
    - `close(): void`
  - `state.ts`: `readPaused(): Set<string>`, `setPaused(id: string, paused: boolean): void` (file `statePath()`; missing or corrupt file → empty set)

- [ ] **Step 1:** Failing tests (each uses a temp `HQ_HOME`):
  - `start/finish round-trip` (row fields exact).
  - `skip records reason`.
  - `recent orders newest first and limits`.
  - `lastReal ignores skipped`.
  - `prune deletes only rows older than 90 days`.
  - **Concurrent access (Review Focus 3):** two `History` instances on the same file; one does 200 `start`/`finish` pairs while the other loops `recent()` → no throw.
  - `setPaused persists across reads`; `corrupt state.json → empty set`.
- [ ] **Step 2:** Run `bun test test/history.test.ts test/state.test.ts`. Expected: FAIL.
- [ ] **Step 3:** Implement with `bun:sqlite` prepared statements.
- [ ] **Step 4:** Run `bun test`. Expected: PASS.
- [ ] **Step 5:** Commit: `feat: run history (sqlite) and hold state`

### Task 4: Runner (`hq run`)

**Files:**
- Create: `src/runner.ts`, `src/log.ts`, `test/runner.test.ts`

**Interfaces:**
- Consumes: `Job`, `loadConfig` (Task 1); `inQuiet` (Task 2); `History`, `readPaused` (Task 3).
- Produces:
  - `log.ts`:
    - `appendLog(id: string, chunk: string | Uint8Array): void`
    - `rotateIfNeeded(id: string, maxBytes = 1_048_576): void`
    - `class TailBuffer { constructor(lines = 20); push(chunk: string): void; text(): string }`
  - `runner.ts`:
    - `runJob(job: Job, opts: { manual: boolean; history: History; now?: () => Date; killGraceMs?: number }): Promise<number>` — returns the exit code: 0 for skipped, 124 for timeout, the job's code otherwise.

- [ ] **Step 1:** Failing tests (temp `HQ_HOME`; jobs built in-test, not from TOML):
  - `ok run`: `echo hi` → status `ok`, exit 0, tail `"hi"`. The log contains the `── RUN <id>` header with `· scheduled ──` and the `── END <id> · NOMINAL ·` footer.
  - `failed run`: `exit 3` → status `failed`, returns 3, footer contains `FAULT` and `exit 3`.
  - `timeout`: `sleep 30` with `timeoutSec 1`, `killGraceMs 200` → status `timeout`, returns 124, finishes in under 3 s. A `trap '' TERM; sleep 30` job is also killed (SIGKILL path).
  - `paused skip`: `setPaused(id,true)` → `skip` with reason `paused`, the command is not executed (it would create a marker file; assert absent). With `manual:true` it runs anyway.
  - `quiet skip`: `now` = 02:00 with quiet 23:00–07:00 → reason `quiet`.
  - `overlap skip`: a lock file holding the current PID → reason `overlap`. **Stale lock** with PID 999999 → taken over and runs.
  - **Missing cwd / missing command (Review Focus 2):** `cwd` `/nonexistent` → status `failed`, the tail mentions the path, no throw. Command `definitely-not-a-cmd` → `failed`, exit 127.
  - **Partial lines (Review Focus 4):** `seq 1 50; printf tail-no-newline` → the tail's last line is `tail-no-newline`, there are 20 lines, and the log contains `1\n` through `tail-no-newline`.
  - `rotation`: a pre-filled 1.1 MB log → after the run `<id>.log.1` exists and `<id>.log` is under 1 MB.
- [ ] **Step 2:** Run `bun test test/runner.test.ts`. Expected: FAIL.
- [ ] **Step 3:** Implement:
  - Spawn with `Bun.spawn(["/bin/zsh","-lc",cmd], {cwd, stdout:"pipe", stderr:"pipe"})` and stream both pipes into `appendLog` + `TailBuffer`.
  - A `cwd` that doesn't exist is checked before spawning: write the reason to the log, then finish as `failed` with exit 1.
  - Lock: `fs.openSync(lockPath, "wx")`, with the PID inside; liveness check via `process.kill(pid, 0)`.
  - Call `history.prune()` once per run.
- [ ] **Step 4:** Run `bun test`. Expected: PASS.
- [ ] **Step 5:** Commit: `feat: job runner with hold/quiet/overlap rules, logs and timeouts`

### Task 5: launchd integration

**Files:**
- Create: `src/launchd.ts`, `test/launchd.test.ts`, `test/__snapshots__/` (generated)

**Interfaces:**
- Consumes: `Job`, `launchAgentsDir`, `migratedDir`, `hqHome` (Task 1); `readPaused` (Task 3).
- Produces:
  - `type Exec = (args: string[]) => Promise<{ code: number; stdout: string; stderr: string }>`, plus `realExec: Exec` (runs `launchctl`)
  - `label(id: string): string` → `com.chngu.hq.<id>`
  - `interface PlistCtx { bunPath: string; cliPath: string; hqHome?: string }` and `defaultCtx(): PlistCtx` (bun = `process.execPath`, cli = `<repoRoot>/src/cli.ts`; sets `hqHome` only if `$HQ_HOME` is set)
  - `renderPlist(job: Job, ctx: PlistCtx): string`
  - `interface AgentState { linked: boolean; running: boolean; lastExit: number | null }` and `agentState(id: string, exec: Exec): Promise<AgentState>`
  - `interface SyncPlan { add: string[]; change: string[]; remove: string[] }` and `planSync(jobs: Job[], ctx: PlistCtx): SyncPlan` (compares rendered vs on-disk `com.chngu.hq.*.plist`)
  - `applySync(plan: SyncPlan, jobs: Job[], ctx: PlistCtx, exec: Exec): Promise<void>`
  - `hold(id: string, exec: Exec)`, `resume(job: Job, ctx: PlistCtx, exec: Exec)` (both also update `setPaused`)
  - `uninstall(exec: Exec): Promise<void>`

- [ ] **Step 1:** Failing tests (temp `HQ_LAUNCH_AGENTS_DIR` + `HQ_HOME`; `exec` is a recording fake):
  - `renderPlist every` and `renderPlist at`: snapshot tests (`toMatchSnapshot`). Also assert:
    - `StartInterval` 1800, or `StartCalendarInterval` as an array of `{Hour:6, Minute:55}`;
    - `ProgramArguments` = [bunPath, cliPath, "run", id];
    - `RunAtLoad` false;
    - Std paths → `<hqHome>/logs/_launchd.log`.
  - `planSync`: an empty dir → all `add`. A plist with a different interval → `change`. A stray `com.chngu.hq.old.plist` → `remove`. A `com.chngu.trml.x.plist` in the dir → ignored.
  - `applySync` call order:
    - for change: `bootout gui/<uid>/<label>` then `bootstrap gui/<uid> <path>`;
    - for remove: bootout + file deleted;
    - **held jobs** get their plist written but no bootstrap.
  - `agentState` parses the fixture `launchctl print` text (from `com.chngu.trml.reminders`: `state = not running`, `last exit code = 0`) → `{linked:true,running:false,lastExit:0}`. `state = running` → `running:true`.
  - **Missing service (Review Focus 5):** exec returns code 113 with `Could not find service` → `{linked:false,running:false,lastExit:null}`, no throw.
  - `uninstall`: removes all hq plists, copies `migrated/*.plist` back to the agents dir, and bootstraps them.
- [ ] **Step 2:** Run `bun test test/launchd.test.ts`. Expected: FAIL.
- [ ] **Step 3:** Implement. Write plists with a small XML serializer: keys in fixed order, 2-space indent, XML-escaped strings. `uid` comes from `process.getuid()`.
- [ ] **Step 4:** Run `bun test`. Expected: PASS.
- [ ] **Step 5:** Commit: `feat: launchd plist generation, sync, hold/resume, uninstall`

### Task 6: CLI

**Files:**
- Create: `src/cli.ts` (shebang `#!/usr/bin/env bun`), `src/snapshot.ts`, `test/cli.test.ts`

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces:
  - `snapshot.ts`:
    - `interface JobView { job: Job; held: boolean; agent: AgentState; synced: boolean; last: RunRow | null; recent: RunRow[]; running: boolean; next: Date | null }`
    - `interface Snapshot { jobs: JobView[]; configError: string | null; at: Date }`
    - `loadSnapshot(deps: { exec: Exec; history: History; now: Date; previous?: Snapshot }): Promise<Snapshot>` — on `ConfigError`, keep `previous.jobs` and set `configError`
  - `summarize(s: Snapshot): { systems: number; nominal: number; fault: number; standby: number }` (counting rules: spec §6 summary bar)
  - CLI subcommands, exactly as spec §8. `status` prints one line per job: `<id>  <OK|FAULT|HOLD|NEVER>  last <hh:mm>  next <hh:mm|—>`. `--json` prints `JobView[]`.

- [ ] **Step 1:** Failing tests: run `bun src/cli.ts` as a subprocess with temp env vars and a temp `jobs.toml` via `HQ_CONFIG`.
  - `status --json` lists jobs.
  - `run <id> --manual` exits with the job's code and the run appears in `status`.
  - `run unknown` exits 2 with a stderr message.
  - `hold`/`resume` toggle `HOLD` in `status`.
  - `sync --yes` writes plists into the temp agents dir. Use the fake exec, injected when `HQ_FAKE_LAUNCHCTL=1`: it records calls to `$HQ_HOME/launchctl.calls` and always succeeds.
  - `summarize` counts from a fixture snapshot.
- [ ] **Step 2:** Run `bun test test/cli.test.ts`. Expected: FAIL.
- [ ] **Step 3:** Implement. `sync` without `--yes` prints the plan and asks `Apply? [y/N]` on stdin. `logs -f` tails using `fs.watch`.
- [ ] **Step 4:** Run `bun test`. Expected: PASS. Then `bun link` and confirm `which hq` resolves.
- [ ] **Step 5:** Commit: `feat: hq CLI (status, run, logs, hold, resume, sync, uninstall)`

### Task 7: TUI dashboard

**Files:**
- Create: `src/tui/theme.ts`, `src/tui/Dashboard.tsx`, `src/tui/JobRow.tsx`, `src/tui/DetailPanel.tsx`, `test/tui/dashboard.test.tsx`, `test/fixtures/snapshot.ts`

**Interfaces:**
- Consumes: `Snapshot`, `JobView`, `summarize` (Task 6); `formatCountdown`, `describeSchedule` (Task 2).
- Produces:
  - `<Dashboard snapshot={Snapshot} selected={number} now={Date} />` (pure render, no I/O)
  - `theme.ts` exports the palette from Global Constraints plus `glyph(view: JobView): "◉" | "◆" | "○" | "⚠"`

- [ ] **Step 1:** Failing tests with `ink-testing-library` `render(...).lastFrame()`. The fixture has four jobs: nominal, fault (exit 1), held, and not-synced (whose last run was `ok`, so it counts as nominal).
  - The frame contains `◢ H Q`, `SYSTEMS  04`, `NOMINAL  02`, `FAULT  01` and `STANDBY  01`.
  - Uppercase ids are listed.
  - The fault row contains `◆` and `FAULT · EXIT 1`. The held row contains `○` and `HOLD`. The not-synced row contains `⚠ NOT SYNCED`.
  - The nominal row contains `T–00:27:52` for `next − now` = 1 672 000 ms.
  - History strip: `▁` for ok, `█` for failed, `·` for skipped, at most 10 marks.
  - The detail panel for `selected=0` shows `cmd`, `dir`, `quiet     23:00 – 07:00` and the last tail line prefixed `› `.
  - The footer has the exact key line from Global Constraints.
  - **Config error (Review Focus 1):** a snapshot with `configError: "jobs.eink: unknown key colour"` and previous jobs → the banner `CONFIG ERROR — jobs.eink: unknown key colour` appears and the job rows still render.
- [ ] **Step 2:** Run `bun test test/tui`. Expected: FAIL.
- [ ] **Step 3:** Implement to match the spec §6 mockup: two lines per job plus a blank line, 5-column left margin, three `─` dividers sized to `useStdout().stdout.columns`, capped at 90.
- [ ] **Step 4:** Run `bun test`. Expected: PASS.
- [ ] **Step 5:** Commit: `feat: sci-fi dashboard view`

### Task 8: TUI app, log view and actions

**Files:**
- Create: `src/tui/LogView.tsx`, `src/tui/App.tsx`, `test/tui/app.test.tsx`
- Modify: `src/cli.ts` (no args → `render(<App/>)`)

**Interfaces:**
- Consumes: `Dashboard` (Task 7); `loadSnapshot` (Task 6); `logPath` (Task 1).
- Produces:
  - `<App deps={{ exec: Exec; history: History; spawnCli: (args: string[]) => void; clock?: () => Date }} />`
  - `<LogView id={string} follow={boolean} />`

- [ ] **Step 1:** Failing tests (fake deps, `stdin.write` for keys):
  - `↓` moves the selection: the detail panel changes job.
  - `⏎` shows LogView: the frame contains the `── RUN` header from a fixture log, with RUN/END lines styled; `esc` returns to the dashboard.
  - `R` calls `spawnCli(["run", id, "--manual"])` and opens LogView with follow on.
  - `P` on a non-held job calls `spawnCli(["hold", id])`; on a held job, `["resume", id]`.
  - `S` shows the sync plan (`+ add`, `~ change`, `- remove` lines); `Y` calls `spawnCli(["sync","--yes"])`, any other key cancels.
  - `Q` exits.
- [ ] **Step 2:** Run `bun test test/tui/app.test.tsx`. Expected: FAIL.
- [ ] **Step 3:** Implement:
  - Snapshot reload every 2000 ms; clock tick every 1000 ms (for countdowns and the `◉` pulse while `running`).
  - LogView keys: `↑↓ PgUp PgDn g G esc`; when `follow` is on, re-read the log on `fs.watch` change.
  - `spawnCli` defaults to `Bun.spawn([process.execPath, cliPath, ...args], {stdio:["ignore","ignore","ignore"]}).unref()`.
- [ ] **Step 4:** Run `bun test`. Expected: PASS. Manually: `HQ_HOME=$(mktemp -d) HQ_FAKE_LAUNCHCTL=1 hq` with the sample `jobs.toml` renders and responds to keys.
- [ ] **Step 5:** Commit: `feat: interactive TUI with log view, run now, hold and sync`

### Task 9: Real jobs and migration (manual acceptance)

**Files:**
- Create: `jobs.toml` (exactly the spec §4 example), `README.md` (usage, keys, env overrides, the Bluetooth permission note, uninstall)

- [ ] **Step 1:** Add only `eink-weather` and `eink-wipe` to `jobs.toml`. Run `hq sync` and confirm. `launchctl print gui/$UID/com.chngu.hq.eink-weather` shows the service.
- [ ] **Step 2:** **Bluetooth check:** `launchctl kickstart gui/$UID/com.chngu.hq.eink-weather`.
  - Expected: `hq logs eink-weather` ends with `pushed (battery … )` and the history status is `ok`.
  - If it fails with a Bluetooth or authorization error: stop. Report the exact log lines to the owner, and follow spec §10 (grant Bluetooth to the binary named in System Settings → Privacy & Security → Bluetooth) before continuing.
- [ ] **Step 3:** Migrate `trml-reminders`:
  - add it to `jobs.toml`;
  - `launchctl bootout gui/$UID/com.chngu.trml.reminders`;
  - `mv ~/Library/LaunchAgents/com.chngu.trml.reminders.plist migrated/`;
  - `hq sync --yes`.
  - Expected: within 5 min `hq status` shows `trml-reminders  OK`.
- [ ] **Step 4:** Same as Step 3 for `tailscale-watchdog`, using `launchctl kickstart` to avoid waiting an hour. Expected: `OK`.
- [ ] **Step 5:** Open `hq`. The owner checks the look against spec §6.
- [ ] **Step 6:** Commit: `feat: register e-ink, reminders and tailscale jobs`
