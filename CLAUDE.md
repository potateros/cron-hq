# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

homelab-hq is the owner's command centre for the scheduled jobs on this Mac. The command is `hq`. It is written in Rust with ratatui and crossterm (it was Bun/TypeScript until the rewrite).

- `jobs.toml` is the registry.
- `hq sync` writes a `com.chngu.hq.<id>` LaunchAgent for each job. Each agent runs `/Users/chngu/.local/bin/hq run <id>`, the runner.
- The TUI reads the run history and logs that the runner writes.

The README has the user-facing documentation. The design spec and plan are in `docs/superpowers/`.

## This is a live system

launchd runs the **installed** binary at `~/.local/bin/hq`, not this checkout. So source edits are **not** live until you reinstall:

```sh
cargo install --path . --root ~/.local --locked --force
```

- **Tests first:** `cargo test --locked` and `cargo clippy --all-targets --locked` must be clean before you reinstall. A crash in the runner stops real jobs, including the e-ink push and the Reminders export.
- **Repo path is baked in:** the binary embeds this repo's path (`CARGO_MANIFEST_DIR`) for `jobs.toml` and `migrated/`. If you move the repo, reinstall; otherwise `hq` prints a "config not found … reinstall" error.
- **No sync after a reinstall:** the plists embed only the binary path, so reinstalling needs no `hq sync`.
- **Ask first** before any of these, unless the user asked for it:
  - `hq sync`, `hq uninstall`, `hq hold` or `hq resume`;
  - `launchctl` bootstrap, bootout or kickstart;
  - editing the real `jobs.toml`;
  - reinstalling the binary.
- **`hq uninstall`** removes every hq agent and restores the plists in `migrated/`.
- **Manual testing:** use `HQ_HOME=$(mktemp -d) HQ_FAKE_LAUNCHCTL=1 HQ_LAUNCH_AGENTS_DIR=$(mktemp -d) HQ_CONFIG=<temp toml>`. Never point a test at `~/Library`.
- **Safe to run:** `hq status` and `hq logs <id>`. Both are read-only.

## Commands

```sh
cargo test --locked                       # all tests; they use temp dirs + fake launchctl
cargo test <name>                         # a single test (substring of its name)
cargo clippy --all-targets --locked       # must be clean
cargo run --example screenshots           # regenerate docs/images/*.png after any TUI change
HQ_HOME=$(mktemp -d) HQ_FAKE_LAUNCHCTL=1 cargo run   # TUI against an empty home, no launchd
```

## Dependencies

`vendor/iana-time-zone-stub` replaces the `iana-time-zone` crate so the binary doesn't link CoreFoundation (saves 4.5 MB RSS). chrono still reads `TZ` and `/etc/localtime`; only its last-resort fallback becomes UTC.

## Architecture

- `src/main.rs`: command dispatch. The commands are `status`, `run`, `logs`, `hold`, `resume`, `sync` and `uninstall`; with no command it opens the TUI. A config error gives a one-line message and exit code 1. `run` also records a config error as a failed run, so faults are visible under launchd.
- `src/lib.rs`: library root, so `examples/screenshots.rs` and tests reuse the modules.
- `src/config.rs`: strict `jobs.toml` parsing. Any unknown key, a bad duration, both or neither of `every`/`at` (unless `manual = true`, which takes neither: an on-demand job with no LaunchAgent, sorted first), or a bad id gives `ConfigError("jobs.<id>: …")`. The runner re-reads it every run.
- `src/runner.rs`:
  1. Checks hold, then quiet hours, then the overlap lock; each skip is recorded with its reason.
  2. Closes rows that a dead runner left `running`.
  3. Runs `/bin/zsh -lc` in a **detached process group**, with the user tool dirs on PATH.
  4. Output streams to the log and a 20-line tail buffer.
  5. On timeout, sends SIGTERM to the group, then SIGKILL after the grace period.
  6. Forwards SIGTERM, SIGINT and SIGHUP from launchd to the group, and still records the run.
  7. Stops waiting for pipes 2 s after the shell exits.
  8. The runner must never panic: every failure path ends in a history finish and an `END` log line.
- `src/history.rs`: rusqlite in WAL mode with a busy timeout. One `runs` table, pruned after 90 days. Skipped rows are ignored when finding the last real run.
- `src/launchd.rs`: deterministic plist rendering, so the sync plan can compare file contents; sync plan/apply touch only `com.chngu.hq.*` agents; hold, resume, uninstall; parses `launchctl print` (top-level keys only). Every launchctl call goes through an injected `Launchctl`, real or fake (`HQ_FAKE_LAUNCHCTL`).
- `src/schedule.rs`: quiet windows, next-run estimates, countdowns.
- `src/log.rs`: per-job log append, rotation and the tail buffer.
- `src/snapshot.rs`: everything the TUI shows, in one read. On a config error it keeps the previous job list.
- `src/state.rs`: held jobs in `state.json`.
- `src/stacks.rs`: `[stacks.*]` from `jobs.toml` (TUI only: config parsing accepts the table but never validates it, so a stack mistake can't stop a job). Wraps `docker compose config/ps` and `docker stats` behind an injected exec; the compose config is cached by file mtimes.
- `src/paths.rs`: every path, with the env overrides `HQ_HOME`, `HQ_CONFIG`, `HQ_LAUNCH_AGENTS_DIR` and `HQ_MIGRATED_DIR`.
- `src/tui/`:
  - `mod.rs`: event loop and `App` state. Views are jobs and services (`Tab`); modes are dash, log, help, sync and stack (the `Z` confirm). Worker threads reload the snapshot every 2 s, docker state every 3 s and `docker stats` every 5 s (it takes ~2 s); the clock ticks every 1 s. A failed load shows an `ERROR —` banner and keeps the last snapshot. Also the global keys and mouse dispatch.
  - `jobs.rs`, `services.rs`, `logview.rs`, `help.rs`: the views.
  - `theme.rs`: colours, margins, widths and text helpers. `canvas.rs`: drawing helpers. `testing.rs`: test helpers.

## TUI rules (owner preferences)

- Sci-fi look in four colours (the `theme.rs` constants: body, accent, fault, hold). Status glyphs are `◉ ◆ ○ ⚠`, and there's a `T–hh:mm:ss` countdown.
- Compact: no blank lines between rows. Four jobs fit 80×24.
- Every row starts and ends on the divider (the `theme.rs` margin and inner-width helpers). Nothing may be wider than the divider: truncate by **display width** with the `theme.rs` / `canvas.rs` helpers (never by `chars().count()` or bytes), or clamp. The latest output wraps to at most 2 lines and ends with `...`.
- Width is the terminal's, capped at the max width in `theme.rs` (90). The TUI uses the full window on the alternate screen.
- **Mouse is required.** Every view registers its clickable regions (hit regions) in `App::hits` while it draws: click selects, clicking the selected row activates it (job → log, log line → expand, service → open URL), the wheel scrolls (×3 in the log), footer keys are clickable, and so are the `Tab ▸` hint and `Y` in confirm prompts. A new interactive element needs a hit region and a test.
- **When you add a symbol or field, add it to `help.rs`**, regenerate the screenshots and update the README.
- TUI tests render into ratatui's `TestBackend` (100 columns) via `tui/testing.rs`. Dashboard tests assert alignment and width properties, not just text.

## Conventions

- **TDD:** for every behaviour change or bug fix, write the failing test first and watch it fail.
- **Test isolation:** tests call `tempHome()` and inject `Exec`. Nothing touches the real home or launchd.
- **Jobs:** job code lives in its own project. hq only knows `cwd` and `command`. To add a job, follow `skills/hq-add-job/SKILL.md`; it's symlinked into `~/.claude/skills`.
- **Commits:** use conventional messages (`feat:`, `fix:`, `chore:`, `docs:`). Push only when asked.

## Rollback to the TypeScript version

The last TypeScript commit is tagged `ts-final`. From a separate worktree (or after stashing your changes):

```sh
git checkout ts-final
bun install && bun link
bun src/cli.ts sync --yes      # rewrites the plists to run bun src/cli.ts
rm ~/.local/bin/hq             # so `hq` resolves to the bun-linked one
```
