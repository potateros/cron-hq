# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

homelab-hq is the owner's command centre for the scheduled jobs on this Mac. The command is `hq`. It uses Bun, TypeScript and Ink 8 with React 19.

- `jobs.toml` is the registry.
- `hq sync` writes a `com.chngu.hq.<id>` LaunchAgent for each job. Each agent runs `bun src/cli.ts run <id>`, the runner.
- The TUI reads the run history and logs that the runner writes.

The README has the user-facing documentation. The design spec and plan are in `docs/superpowers/`.

## This is a live system

The LaunchAgents in `~/Library/LaunchAgents` run this checkout's `src/cli.ts` on a schedule. Every edit to `src/` is live from the next run, which is within 5 minutes. A crash in the runner stops real jobs, including the e-ink push and the Reminders export.

- **Tests:** `bun test` must be green before you commit.
- **Ask first** before any of these, unless the user asked for it:
  - `hq sync`, `hq uninstall`, `hq hold` or `hq resume`;
  - `launchctl` bootstrap, bootout or kickstart;
  - editing the real `jobs.toml`.
- **`hq uninstall`** removes every hq agent and restores the plists in `migrated/`.
- **Manual testing:** use `HQ_HOME=$(mktemp -d) HQ_FAKE_LAUNCHCTL=1 HQ_LAUNCH_AGENTS_DIR=$(mktemp -d) HQ_CONFIG=<temp toml>`. Never point a test at `~/Library`.
- **Safe to run:** `hq status` and `hq logs <id>`. Both are read-only.
- **Moving the repo:** the plists pin absolute paths to `bun` and `src/cli.ts`. After a move, run `bun link` and `hq sync --yes`.

## Commands

```sh
bun install
bun test                                  # 125 tests; all use temp dirs + fake launchctl (test/helpers.ts tempHome())
bun test test/runner.test.ts              # one file
bun test -t "SIGTERM"                     # by test name
bunx tsc --noEmit                         # typecheck (known: stdout.columns/rows typing errors in Dashboard/LogView)
bun tools/screenshots.tsx                 # regenerate docs/images/*.png (headless Chrome) after any TUI change
HQ_HOME=$(mktemp -d) HQ_FAKE_LAUNCHCTL=1 bun src/cli.ts   # TUI against an empty home, no launchd
```

`hq` on PATH is `bun link` → `src/cli.ts`.

## Architecture

- `src/cli.ts`: command dispatch. The commands are `status`, `run`, `logs`, `hold`, `resume`, `sync` and `uninstall`; with no command it opens the TUI. A `ConfigError` gives a one-line message and exit code 1. `run` also records a config error as a failed run, so faults are visible under launchd.
- `src/config.ts`: strict `jobs.toml` parsing (`Bun.TOML`). Any unknown key, a bad duration, both or neither of `every`/`at` (unless `manual = true`, which takes neither: an on-demand job with no LaunchAgent, sorted first), or a bad id gives `ConfigError("jobs.<id>: …")`. The runner re-reads it every run.
- `src/runner.ts` (`runJob`):
  1. Checks hold, then quiet hours, then the overlap lock; each skip is recorded with its reason.
  2. `failStale` closes rows that a dead runner left `running`.
  3. Runs `/bin/zsh -lc` in a **detached process group**, with `jobEnv()` putting the user tool dirs on PATH.
  4. Output streams to the log and a 20-line `TailBuffer`.
  5. On timeout, sends SIGTERM to the group, then SIGKILL after `killGraceMs`.
  6. Forwards SIGTERM, SIGINT and SIGHUP from launchd to the group, and still records the run.
  7. Stops waiting for pipes 2 s after the shell exits (`DRAIN_MS`).
  8. The runner must never throw: every failure path ends in `history.finish` and an `END` log line.
- `src/history.ts`: `bun:sqlite` in WAL mode with `busy_timeout`. One `runs` table, pruned after 90 days. `lastReal` ignores skipped rows.
- `src/launchd.ts`:
  - `renderPlist`: deterministic output, so `planSync` can compare file contents.
  - `planSync` / `applySync`: touch only `com.chngu.hq.*` agents.
  - `hold`, `resume`, `uninstall`.
  - `agentState`: parses `launchctl print`, top-level keys only.
  - Every launchctl call goes through the injected `Exec`. `src/exec.ts` picks the real or fake one via `HQ_FAKE_LAUNCHCTL`.
- `src/snapshot.ts`: `loadSnapshot` is everything the TUI shows, in one read. On a `ConfigError` it keeps the previous job list.
- `src/stacks.ts`: `[stacks.*]` from `jobs.toml` (TUI only: `parseConfig` accepts the table but never validates it, so a stack mistake can't stop a job). Wraps `docker compose config/ps` and `docker stats` behind an injected `DockerExec`; the compose config is cached by file mtimes.
- `src/paths.ts`: every path, with the env overrides `HQ_HOME`, `HQ_CONFIG`, `HQ_LAUNCH_AGENTS_DIR` and `HQ_MIGRATED_DIR`.
- `src/tui/`:
  - `App`: views are jobs and services (`Tab`); modes are dash, log, help, sync and stack (the `Z` confirm). It reloads the snapshot every 2 s, docker state every 3 s and `docker stats` every 5 s (it takes ~2 s), and ticks the clock every 1 s. A failed load shows an `ERROR —` banner and keeps the last snapshot.
  - The other components: `Dashboard`, `JobRow`, `DetailPanel`, `LogView`, `ServicesView` and `HelpView`.
  - `theme.ts`: colours, `MARGIN`, `INDENT`, `innerWidth()` and `clampLines()`.

## TUI rules (owner preferences)

- Sci-fi look in four colours (`theme.color`: body, accent, fault, hold). Status glyphs are `◉ ◆ ○ ⚠`, and there's a `T–hh:mm:ss` countdown.
- Compact: no blank lines between rows. Four jobs fit 80×24.
- Every row starts and ends on the divider (`MARGIN`, `innerWidth`). Nothing may be wider than the divider: truncate (`wrap="truncate-end"`) or clamp. The latest output wraps to at most 2 lines and ends with `...`.
- Width is the terminal's, capped at `MAX_WIDTH` = 90.
- **When you add a symbol or field, add it to `HelpView`**, regenerate the screenshots and update the README.
- TUI tests use `ink-testing-library` (100 columns). Dashboard tests assert alignment and width properties, not just text.

## Conventions

- **TDD:** for every behaviour change or bug fix, write the failing test first and watch it fail.
- **Test isolation:** tests call `tempHome()` and inject `Exec`. Nothing touches the real home or launchd.
- **Jobs:** job code lives in its own project. hq only knows `cwd` and `command`. To add a job, follow `skills/hq-add-job/SKILL.md`; it's symlinked into `~/.claude/skills`.
- **Commits:** use conventional messages (`feat:`, `fix:`, `chore:`, `docs:`). Push only when asked.
