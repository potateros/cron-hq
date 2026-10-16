# homelab-hq

A terminal command centre for the small scheduled jobs on this Mac. `jobs.toml` is the registry.
`hq sync` turns each job into a `com.chngu.hq.<id>` LaunchAgent, and every agent runs
`bun src/cli.ts run <id>`. The runner is what applies hold, quiet hours, the overlap lock and
timeouts, and it records logs and run history. The job's own code stays in its own project.

```sh
bun install
bun link              # puts `hq` on PATH (~/.bun/bin/hq)
bun test
```

## Commands

```sh
hq                    # TUI dashboard
hq status [--json]    # one line per job: OK / FAULT / HOLD / NEVER, last and next run
hq run <job> [--manual]   # run now (--manual ignores hold and quiet hours)
hq logs <job> [-f]    # print the job's log, -f to follow
hq hold <job>         # unload the agent and mark it held
hq resume <job>
hq sync [--yes]       # write/update/remove LaunchAgents to match jobs.toml
hq uninstall          # remove all hq agents and restore the plists in migrated/
```

TUI keys: `↑↓` select, `enter` log, `R` run now, `P` hold/resume, `S` sync (`Y` to apply), `H` help (what each symbol means), `Q` quit.
In the log view: `↑↓ PgUp PgDn`, `g` top, `G` bottom, `esc` back.

## jobs.toml

```toml
[jobs.eink-weather]
description = "Punggol weather → nRF e-ink"
cwd         = "~/Developer/potateros/nrf5-eink"
command     = "uv run python -m epd.cli --push"   # run with /bin/zsh -lc
every       = "30m"                              # or: at = ["06:55", "19:30"]
quiet       = "23:00-07:00"                      # optional; scheduled runs are skipped
timeout     = "5m"                               # SIGTERM, then SIGKILL after 10 s
```

The runner reads `jobs.toml` each time a job runs, so you only need `hq sync` after adding or
removing a job or changing its schedule. Commands get `~/.local/bin`, `~/.bun/bin`,
`/opt/homebrew/bin` and `/usr/local/bin` prepended to PATH, because launchd does not read `~/.zshrc`.

Data lives in `~/Library/Application Support/hq/`: `logs/<id>.log` (rotated), `history.db`
(SQLite) and `state.json` (holds). launchd's own stdout/stderr goes to `logs/_launchd.log`.

## Permissions (TCC)

macOS gives privacy permissions to the *responsible* process. Under launchd, that is `bun`,
not your terminal. Bluetooth jobs (the e-ink push) fail with
`Bluetooth access is denied by the user for the current application` until `bun`
(`~/.bun/bin/bun`) is allowed in System Settings → Privacy & Security → Bluetooth. Reinstalling
or upgrading bun may need the grant again. The reminders exporter handles Reminders access itself
by going through `osascript`.

## Migration and uninstall

The `com.chngu.trml.*` LaunchAgents that hq replaced were moved to `migrated/` (gitignored).
`hq uninstall` removes every hq agent and puts those plists back.

## Environment overrides (tests and dry runs)

`HQ_HOME`, `HQ_CONFIG`, `HQ_LAUNCH_AGENTS_DIR`, `HQ_MIGRATED_DIR`, and `HQ_FAKE_LAUNCHCTL=1`
(an in-memory launchctl). For example: `HQ_HOME=$(mktemp -d) HQ_FAKE_LAUNCHCTL=1 hq`.
