---
name: hq-add-job
description: Use when the user wants to schedule, register, or "add to hq" a recurring script, cron job, LaunchAgent or background task on this Mac, or to change or remove an hq job. hq (homelab-hq) is the owner's launchd-backed job command centre; every scheduled job on this machine belongs in its jobs.toml.
---

# Add a job to hq

hq (`~/Developer/potateros/homelab-hq`, command `hq`) is the one place scheduled jobs live on this Mac:

- `jobs.toml` is the registry.
- `hq sync` turns each entry into a `com.chngu.hq.<id>` LaunchAgent that runs `hq run <id>`.
- The runner applies quiet hours, holds, overlap locks and timeouts, and records logs and history.

The job's code stays in its own project. Never hand-write a LaunchAgent or crontab line for a recurring job. Add it to hq instead.

## 1. Pin down the job

Get these from the user or the project. Ask only for what you can't infer:

| key | required | format | notes |
|---|---|---|---|
| id (table name) | yes | `[a-z0-9-]+` | becomes `[jobs.<id>]`; prefix by project, e.g. `eink-weather`, `trml-reminders` |
| `command` | yes | string | run with `/bin/zsh -lc` from `cwd` |
| `every` **or** `at` | exactly one | `every = "30m"` (`Ns`/`Nm`/`Nh`, min 60s) · `at = ["06:55", "19:30"]` | both or neither is an error |
| `cwd` | no | path, `~` ok | defaults to `$HOME`; must be a directory |
| `quiet` | no | `"HH:MM-HH:MM"` | scheduled runs inside it are skipped (wraps midnight); manual runs still go |
| `timeout` | no | `"5m"` | default 10m; SIGTERM, then SIGKILL 10 s later |
| `description` | no | string | shown on the dashboard; keep it short |

No other keys exist: an unknown key is a config error that faults **every** job until fixed.

## 2. Check the command works the way launchd will run it

launchd gives a bare environment. hq prepends `~/.local/bin`, `~/.bun/bin`, `/opt/homebrew/bin` and `/usr/local/bin` to PATH. Anything that only `~/.zshrc` sets is missing. Dry-run the command the same way:

```sh
cd <cwd> && env -i HOME=$HOME PATH="$HOME/.local/bin:$HOME/.bun/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin" /bin/zsh -lc '<command>'
```

Only do this when running the job is harmless, e.g. it doesn't push to a device or send messages. Otherwise ask first.

- **Exit status:** the job must exit 0 on success and non-zero on failure, because hq uses the exit code for OK and FAULT.
- **Output:** the last line of output is shown on the dashboard, so it should say what happened.
- **macOS privacy (TCC):** under launchd, Bluetooth, Reminders, Calendar, Photos and so on are granted to the responsible process, `~/.bun/bin/bun`, not to the terminal. Bluetooth is already granted to bun. For anything else, warn the user that the first scheduled run may fail with "denied", and tell them where to grant it: System Settings → Privacy & Security.

## 3. Add it

1. Read `~/Developer/potateros/homelab-hq/jobs.toml`. Make sure the id isn't taken. Append the block in the file's existing column-aligned style.
2. Validate before anything is loaded:
   ```sh
   cd ~/Developer/potateros/homelab-hq && hq status
   ```
   A `hq: jobs.<id>: …` error means the file is wrong. Fix it before going further.
3. If the job replaces an existing LaunchAgent or cron entry, retire the old one so the job doesn't run twice:
   - LaunchAgent: `launchctl bootout gui/$(id -u)/<label>`, then move its plist into `homelab-hq/migrated/` so `hq uninstall` can restore it.
   - cron: remove the line.
4. `hq sync --yes`. Expected output: `+ <id>` and then `synced`.
5. Prove it under launchd, not just in your shell:
   ```sh
   launchctl kickstart gui/$(id -u)/com.chngu.hq.<id>
   hq logs <id> | tail        # ends with "── END nnnn · NOMINAL · …"
   hq status                  # <id> shows OK
   ```
   On FAULT, read the log, fix the cause, and kick it again. Common causes: command not on PATH, wrong cwd, TCC denial.
6. Commit `jobs.toml` in the homelab-hq repo (`feat: add <id> job`). Push only if the user asks.

## Changing or removing a job

- **No sync needed:** `command`, `cwd`, `quiet`, `timeout` and `description` are re-read on every run.
- **Changes to `every` or `at`:** run `hq sync --yes`. It reports `~ <id>`.
- **Removing a job:** delete the block, then `hq sync --yes`. It reports `- <id>`, unloads the agent and deletes its plist. Logs and history stay in `~/Library/Application Support/hq/`.
- **Pausing without deleting:** `hq hold <id>`, then `hq resume <id>`. In the TUI, press `P`.

## Don'ts

- Don't schedule anything faster than every 60s; hq rejects it.
- Don't run `hq uninstall` unless asked. It removes **every** hq agent and restores the migrated plists.
- Don't give two jobs that use the same exclusive resource overlapping schedules. Locks are per job, so overlapping runs would collide. Example: the e-ink BLE device is shared by `eink-weather` and `eink-wipe`.
