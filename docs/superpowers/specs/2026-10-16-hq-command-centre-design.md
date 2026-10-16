# hq: command centre for scheduled jobs (design)

Date: 2026-10-16
Status: draft, awaiting owner review

## 1. Purpose

One place on the Mac mini for all the owner's small scheduled terminal jobs, with a sci-fi-styled React (Ink) terminal UI to see and control them.

**Success means:**
- Every small scheduled job is defined in one file (`jobs.toml`) and installed from it.
- One glance at `hq` shows each job's state, last result, next run and recent history.
- Logs, run-now and pause/resume all work from the TUI without editing launchd files by hand.
- Jobs keep running when the TUI is closed, and catch up after the Mac sleeps.

**Decided with the owner:**
- `hq` owns the jobs: it is the registry and installs LaunchAgents. It is not just a viewer.
- Job code stays in its own project. `jobs.toml` only points at a command and directory.
- Version 1 features: status at a glance, logs + run now, pause/resume, run history.
- Look: sci-fi console, generous white space (mockup in §6).

**Non-goals (v1):**
- KOReader anything: explicitly out of scope.
- Always-on services (Hermes gateway, homelab Docker containers, Terminus): not managed or monitored.
- Failure notifications (possible later; see §10).
- Editing `jobs.toml` from the TUI.

## 2. Approach

One LaunchAgent per job, each running a wrapper (`hq run <job>`) that records history and logs.

**Rejected alternatives:**
- A single resident scheduler daemon: one crash stops every job, it handles sleep/wake worse, and it needs its own monitoring.
- crontab: no catch-up after sleep, deprecated direction on macOS, no gain over launchd.

## 3. Stack and layout

- Bun 1.3 + TypeScript, Ink (React for terminals), `bun:sqlite`, Bun's built-in TOML import/parse.
- Repo: `~/Developer/potateros/hq`, installed globally with `bun link` → command `hq`. (`cc` = C compiler and `jobs` = shell builtin, so both names are taken.)

```
hq/
  jobs.toml            the registry (in git)
  src/
    config.ts          parse + validate jobs.toml
    schedule.ts        durations, quiet windows, next-run calculation
    launchd.ts         plist generation, load/unload, launchctl state
    runner.ts          `hq run <job>`
    history.ts         SQLite access
    state.ts           paused set (state.json)
    tui/               Ink app: Dashboard, LogView, components
    cli.ts             entry point and subcommands
  test/
  migrated/            backups of replaced LaunchAgents (gitignored)
  docs/

~/Library/Application Support/hq/
  history.db
  state.json           { "paused": ["job-id", …] }
  logs/<job>.log       rotated to <job>.log.1 at ~1 MB
  locks/<job>.lock
```

Every path above (App Support dir, LaunchAgents dir) can be overridden with environment variables (`HQ_HOME`, `HQ_LAUNCH_AGENTS_DIR`) so tests never touch the real system.

## 4. `jobs.toml`

```toml
[jobs.eink-weather]
description = "Punggol weather → nRF e-ink"
cwd         = "~/Developer/potateros/nrf5-eink"
command     = "uv run python -m epd.cli weather --push"
every       = "30m"
quiet       = "23:00-07:00"
timeout     = "5m"

[jobs.eink-wipe]
description = "Daily full clear of the e-ink (ghosting)"
cwd         = "~/Developer/potateros/nrf5-eink"
command     = "uv run python -m epd.cli weather --push --wipe"
at          = ["06:55"]
timeout     = "5m"

[jobs.trml-reminders]
description = "Apple Reminders → TRMNL sidecar"
command     = "~/Developer/potateros/trml-byos/host/reminders-export.sh"
every       = "5m"

[jobs.tailscale-watchdog]
description = "Restart Tailscale if it's down"
command     = "~/Developer/potateros/trml-byos/host/tailscale-watchdog.sh"
every       = "1h"
```

**Rules:**
- Job id (table key): `[a-z0-9-]+`. LaunchAgent label: `com.chngu.hq.<id>`.
- `command` (required, string): run via the owner's login shell (`/bin/zsh -lc`), so PATH matches the terminal.
- `cwd` (optional, default `~`): `~` is expanded.
- Exactly one of:
  - `every`: duration `<n>s|m|h`, at least 60 s → launchd `StartInterval`.
  - `at`: list of `HH:MM` → launchd `StartCalendarInterval` (daily).
- `quiet` (optional): `HH:MM-HH:MM`, may wrap midnight. Runs that fall inside it are skipped by the runner.
- `timeout` (optional, default `10m`).
- `description` (optional).
- Unknown keys are an error. Validation errors name the job and key.

**Weather interval:** `every = "30m"` matches NEA's 2-hour nowcast refresh; PSI is hourly and Open-Meteo model runs every 1–3 h, so pushing more often only costs battery and flashing. A push takes ~40 s and runs only 07:00–23:00 → 32 a day; the 06:55 wipe adds one slow (~70 s) full clear.

## 5. Runner: `hq run <job> [--manual]`

1. Load config; unknown job → exit 2.
2. Unless `--manual`:
   - paused → record `skipped` (reason `paused`), exit 0;
   - inside `quiet` → record `skipped` (`quiet`), exit 0.
3. Take `locks/<job>.lock` (O_EXCL, containing our PID; a stale lock whose PID isn't alive is taken over). If it's held, record `skipped` (`overlap`), exit 0.
4. Append the header `── RUN 0042 · 2026-10-16 17:30:00 · manual|scheduled ──` to the log. Spawn `/bin/zsh -lc <command>` in `cwd`, with stdout and stderr both appended to the log as produced. Keep the last 20 lines in memory.
5. Timeout: SIGTERM, then SIGKILL after 10 s → status `timeout`.
6. Append the footer `── END 0042 · NOMINAL|FAULT|TIMEOUT · 41s · exit 0 ──`. Insert the run record. Release the lock. Rotate the log if it's over 1 MB (keep one `.1`).
7. The runner's own exit code mirrors the job's, so launchd's `last exit code` agrees.

**History schema (`history.db`):**

```sql
CREATE TABLE runs (
  id INTEGER PRIMARY KEY,
  job TEXT NOT NULL,
  trigger TEXT NOT NULL,          -- scheduled | manual
  started_at INTEGER NOT NULL,    -- unix ms
  ended_at INTEGER,
  exit_code INTEGER,
  status TEXT NOT NULL,           -- running | ok | failed | timeout | skipped
  reason TEXT,                    -- skipped: paused | quiet | overlap
  tail TEXT                       -- last 20 output lines
);
CREATE INDEX runs_job_started ON runs(job, started_at DESC);
```

A row is inserted as `running` at start and updated at the end, so the TUI can show live runs. Rows older than 90 days are pruned on each run.

## 6. TUI (`hq`)

Ink app, redrawn every 1 s for the countdowns. Data is reloaded every 2 s from `history.db`, `state.json` and `launchctl print gui/<uid>/<label>` (state, last exit code).

```
   ◢ H Q                                                    17:32:08  ·  FRI 16 OCT
   ───────────────────────────────────────────────────────────────────────────────

     SYSTEMS  04          NOMINAL  03          FAULT  01          STANDBY  00


     ◉  EINK-WEATHER                                        T–00:27:52
        every 30m  ·  last 17:30  ·  41s                     ▁ ▁ ▁ █ ▁ ▁ ▁ ▁ ▁ ▁

     ◆  TAILSCALE-WATCHDOG                    FAULT · EXIT 1   T–00:32:52
        every 1h   ·  last 17:05  ·  9s                      ▁ ▁ ▁ ▁ ▁ █ ▁ █ ▁ █

     ○  EINK-WIPE                                            HOLD
        daily 06:55  ·  paused                                ▁ ▁ ▁ ▁ ▁ ▁

   ───────────────────────────────────────────────────────────────────────────────

     ▸  EINK-WEATHER  —  Punggol weather → nRF e-ink

        cmd       uv run python -m epd.cli weather --push
        dir       ~/Developer/potateros/nrf5-eink
        quiet     23:00 – 07:00          timeout   5m          launchd   linked

        › pushed (battery 2921 mV, clock drift -3s), fallback armed for midnight

   ───────────────────────────────────────────────────────────────────────────────
     ↑↓ select     ⏎ logs     R run     P hold     S sync     Q exit
```

**Visual language:**
- Dark palette: body dim cyan-grey; selected job and key labels bright cyan; FAULT amber/red; HOLD dim grey.
- Glyphs: `◉` nominal (pulses while running), `◆` fault, `○` hold, `⚠` not synced. Letter-spaced title `◢ H Q`.
- Generous spacing: two lines per job plus a blank line, wide margins, three dividers.

**Per-job fields:**
- Status glyph + id (uppercase).
- Schedule summary, last run time + duration.
- Countdown `T–hh:mm:ss` to the next run (respecting quiet hours), or `HOLD` / `RUNNING`.
- History strip: last 10 runs; `▁` ok, `█` (red) failed/timeout, dim `·` skipped.

**Summary bar:**
- SYSTEMS = all jobs; NOMINAL = last real run ok; FAULT = last real run failed/timeout; STANDBY = paused or never run.

**Not synced:** a job whose LaunchAgent is missing or differs from the generated plist shows `⚠ NOT SYNCED`.

**Keys:**
- `↑↓` select.
- `⏎` log view.
- `R` run now (spawns `hq run <job> --manual` detached, then opens the log view following live).
- `P` hold/resume.
- `S` sync: shows the add/change/remove diff, confirm with `Y`.
- `Q` exit.

**Log view:**
- Full screen, follows the tail.
- `↑↓ PgUp PgDn` scroll, `g/G` top/bottom, `esc` back.
- RUN/END lines highlighted, END coloured by status.

The layout fits ~6 jobs on a 40-row terminal; the list scrolls beyond that.

## 7. launchd integration

**Generated plist per job:**
- `Label`.
- `ProgramArguments` = [absolute bun path, absolute `src/cli.ts` path, `run`, `<id>`].
- `StartInterval` or `StartCalendarInterval`.
- `RunAtLoad` false.
- `StandardOutPath`/`StandardErrorPath` → `logs/_launchd.log` (the runner's own errors only; job output goes to the job log).
- `EnvironmentVariables` with `HQ_HOME` if set.

**`hq sync`:**
- Diffs `jobs.toml` against the installed `com.chngu.hq.*` plists.
- Writes changed plists, then `launchctl bootout` + `bootstrap gui/<uid>` for changed or new jobs, and boots out + deletes removed ones.
- Skips loading paused jobs (writes their plist but leaves them booted out).
- Never touches labels outside `com.chngu.hq.`.

**Hold and resume:**
- Hold = `bootout` + add to `state.json`.
- Resume = `bootstrap` + remove from it.

**`hq uninstall`:** boots out and deletes all `com.chngu.hq.*` plists, then restores the plists in `migrated/` and bootstraps them.

## 8. CLI

```
hq                      TUI
hq status [--json]      one line per job (or JSON), for SSH/agents
hq run <job> [--manual]
hq logs <job> [-f]
hq hold <job> | hq resume <job>
hq sync [--yes]
hq uninstall
```

## 9. Migration

1. **eink-weather (+ eink-wipe).** New jobs; they also answer the Bluetooth question (see risks). Sync → manual run → check the push lands and the history row is `ok` → wait for one scheduled run.
2. **trml-reminders, tailscale-watchdog.** For each:
   - Add the entry with the same interval.
   - `launchctl bootout` the old `com.chngu.trml.*`, move its plist to `hq/migrated/`, `hq sync`.
   - Confirm one scheduled `ok` run.
   - Their old logs in `trml-byos/host/data/` simply stop growing; that repo is unchanged.
3. **Hermes gateway (`ai.hermes.gateway`):** untouched (service).

## 10. Risks and open items

- **Bluetooth permission (TCC).** Under launchd the responsible process is `bun`/`zsh`/`uv`/`python`, not the terminal, so BLE may be denied or prompt once.
  - Verified first, with eink-weather, before migrating anything else.
  - Fallbacks, in order: grant Bluetooth to the reported binary in System Settings; or run the job through a small signed helper app.
- **Mac sleep:** jobs only run while the Mac is awake. Owner to set "Prevent automatic sleeping when the display is off". The e-ink midnight fallback covers gaps.
- **Login shell side effects:** `zsh -lc` sources the owner's profile. If a profile prints output or is slow, add a `shell = "/bin/bash"` per-job override (out of scope until needed).
- **Later:**
  - Failure notification (macOS `osascript` notification after 2 consecutive faults).
  - Managing always-on services.
  - Per-job weekday schedules.

## 11. Testing

**Unit tests (`bun test`):**
- Config validation: bad durations, both or neither of `every`/`at`, unknown keys, bad ids, quiet format.
- `schedule.ts`: quiet windows incl. wrap-past-midnight; next-run for `every` (from the last start) and `at`, both with quiet hours.
- Plist generation: snapshot tests.
- Runner: ok / failed / timeout (fake `sleep` job) / skipped (paused, quiet, overlap with a held lock, stale-lock takeover); log header/footer; rotation.
- History: insert/update/prune against a temp DB.

**TUI:** `ink-testing-library` renders the dashboard and log view from fixture data; assert key text and states (FAULT, HOLD, NOT SYNCED, countdown format).

**Isolation:** all tests set `HQ_HOME` and `HQ_LAUNCH_AGENTS_DIR` to temp dirs and stub `launchctl` via an injectable executor. No test touches the real system.

**Manual acceptance:** the migration steps in §9 for each job.
