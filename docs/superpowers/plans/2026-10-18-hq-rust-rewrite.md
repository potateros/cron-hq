# hq Rust Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Bun/TypeScript/Ink hq with a single Rust binary, using ratatui for the TUI. It keeps full feature parity and the same files on disk, and the live LaunchAgents switch over safely.

**Architecture:** One crate with one binary, `hq`. Small focused modules mirror today's `src/*.ts` one-to-one. There is no async runtime. Background work (docker, launchctl) runs on plain threads that report to the TUI's event loop over an `mpsc` channel. The TUI redraws only when state changes, plus once a second for the clock and countdowns. Changes are detected by `stat` mtimes, never by re-reading files on a timer.

**Tech Stack:**
- Rust 2021 (stable, cargo 1.98)
- `ratatui` 0.29 with `crossterm` 0.28
- `rusqlite` with the `bundled` feature
- `toml` 0.8 and `serde`, `serde_json`
- `chrono`, `libc`, `signal-hook`, `unicode-width`
- dev-only: `tempfile`

**Spec:** `docs/superpowers/specs/2026-10-16-hq-command-centre-design.md` describes the behaviour. `README.md` and `CLAUDE.md` add the features that came after the spec: on-demand jobs, `url`, `[stacks.*]`, the services view and the log cursor. The TypeScript tests in `test/` are the executable spec: every case is ported.

## Global Constraints

- **Files on disk stay byte-compatible with the TS version:**
  - `jobs.toml` keys, and every `ConfigError` message string.
  - The `history.db` schema (table `runs`, index `runs_job_started`, WAL, `busy_timeout` 5000, rows pruned after 90 days).
  - `state.json` as `{"paused": [...]}`, sorted, 2-space indent, written atomically (via `.tmp`).
  - Logs under `logs/<id>.log`, rotated to `.log.1` above 1 MB.
  - Lock files under `locks/<id>.lock` containing the holder's PID.
- **Log framing is exact:**
  - `── RUN 0001 · 2026-10-17 12:12:25 · scheduled ──`
  - `── END 0001 · NOMINAL · 47s · exit 0 ──`
  - END labels: `NOMINAL` / `TIMEOUT` / `FAULT`; `killed` when there is no exit code.
  - Durations look like `41s` or `1m06s`.
- **Environment overrides:** `HQ_HOME`, `HQ_CONFIG`, `HQ_LAUNCH_AGENTS_DIR`, `HQ_MIGRATED_DIR`, `HQ_FAKE_LAUNCHCTL=1`. Defaults:
  - `HQ_HOME`: `~/Library/Application Support/hq`
  - Config: `env!("CARGO_MANIFEST_DIR")/jobs.toml`
  - Migrated plists: `CARGO_MANIFEST_DIR/migrated`
  - The binary knows its repo at compile time, so moving the repo means reinstalling.
- **LaunchAgents:**
  - Label prefix `com.chngu.hq.`.
  - Plist `ProgramArguments` = `[<installed binary>, "run", <id>]`.
  - The binary is installed with `cargo install --path . --root ~/.local`, giving `~/.local/bin/hq`.
  - Plist output is deterministic, so `plan_sync` can compare whole files.
- **Exit codes:** timeout 124, usage 2, config error 1 (message `hq: <msg>` on stderr).
- **Runner environment:** jobs run as `/bin/zsh -lc <command>` in `cwd`, in a new process group. PATH is prefixed with `~/.local/bin`, `~/.bun/bin`, `/opt/homebrew/bin` and `/usr/local/bin`.
- **Runner timings:** SIGTERM on timeout, SIGKILL after a 10 s grace. Stop waiting for the pipes 2 s after the shell exits. The tail buffer keeps 20 lines.
- **UI copy and look are identical to the TS version.** Copy glyphs, key lines, help rows and colours verbatim from `src/tui/*.tsx`: `theme.ts` colours, `MAX_WIDTH` 90, `MARGIN` 2, `INDENT` 3.
- **Measure all text by display width (`unicode-width`), never by byte index.** Slicing a `String` at a byte index panics.
- **Performance targets, measured in Task 11 with the method from the 2026-10-18 measurement:**
  - TUI idle memory ≤ 15 MB.
  - TUI idle CPU ≤ 1% of a core, in the jobs view with stacks configured.
  - `hq run` memory ≤ 5 MB plus the job itself.
  - `hq status` ≤ 20 ms.
- **Live system:** develop in a worktree (`../homelab-hq-rust`, branch `rust-rewrite`). Tests never touch `~/Library` and always use `tempfile` dirs and the fake launchctl. Do not run `hq sync`, `launchctl` or `hq hold`/`hq resume` against the real system, or edit the real `jobs.toml`, without the owner's go-ahead (Task 12).

## Review Focus

1. **An existing `history.db` written by Bun, opened while the old runner may still write to it:** Rust must read the old rows, including `NULL` tail/reason columns, and must wait rather than fail when the database is locked. Test: Task 2, `opens_db_created_by_ts_schema`.
2. **Job output that isn't valid UTF-8:** this covers invalid bytes and multibyte characters split across reads. The log must keep the raw bytes, and the tail must decode lossily without panicking. Test: Task 4, `non_utf8_and_split_multibyte_output`.
3. **Wide characters (`──`, emoji, CJK) in log lines, descriptions and service names:** truncation, wrapping and the log view's `…` must work by display width, with no panic. Tests: Task 9, `truncates_wide_chars_by_width`, and Task 8, `row_with_emoji_description_fits_width`.
4. **Tiny or resized terminals (down to 20×5):** every view must render without panicking. Test: Task 8, `renders_tiny_terminal_without_panic` (jobs, log, help and services).
5. **Docker not running, or the `docker` CLI missing:** the services view shows the error, and the jobs view and its key bindings keep working. Test: Task 10, `docker_missing_shows_error_tui_stays_up`.

---

## File Structure

```
Cargo.toml                 crate `hq`, bin `hq`
src/main.rs                CLI dispatch (hand-rolled args; no clap)
src/paths.rs               Paths (env overrides, every path)
src/config.rs              Job, Schedule, QuietWindow, ConfigError, parse/load
src/schedule.rs            in_quiet, next_run, format_countdown, describe_schedule
src/history.rs             History (rusqlite), RunRow, Status, Trigger
src/state.rs               read_paused, set_paused
src/log.rs                 append_log, rotate_if_needed, TailBuffer
src/runner.rs              run_job, job_env, format_duration, lock
src/launchd.rs             Launchctl trait + real/fake, render_plist, agent_state, sync, hold, resume, uninstall
src/snapshot.rs            JobView, Snapshot, load_snapshot, summarize
src/stacks.rs              Stack, Docker trait, ServiceView, parsing, load_stack_view, totals
src/tui/mod.rs             event loop, App state, key handling, Effects trait
src/tui/theme.rs           colours, widths, clamp_lines, glyph, clock_line, truncate_width
src/tui/jobs.rs            jobs view: header, summary, rows, detail panel, SERVICES line
src/tui/logview.rs         log view: cursor, expand, wrap-all, follow
src/tui/services.rs        services view
src/tui/help.rs            per-view help
examples/screenshots.rs    renders views with demo data to docs/images/*.png
tests/cli.rs               end-to-end CLI tests against the built binary
```

TS files are deleted in Task 12, after cutover, not before.

---

### Task 1: Crate scaffold, paths, config, schedule

**Files:**
- Create: `Cargo.toml`, `src/main.rs` (stub `fn main` that prints the usage line and exits 2), `src/paths.rs`, `src/config.rs`, `src/schedule.rs`
- Modify: `.gitignore` (add `/target`)
- Test: unit tests in each module

**Interfaces:**
- Produces:
  - `paths.rs`:
    - `pub struct Paths { pub home: PathBuf, pub config: PathBuf, pub agents: PathBuf, pub migrated: PathBuf }`
    - `Paths::from_env() -> Paths` and `Paths::temp(root: &Path) -> Paths` (for tests).
    - Methods `db()`, `state()`, `log(id)`, `lock(id)`, `launchd_log()`, `launchctl_calls()`, all returning `PathBuf`.
    - `pub fn expand_home(p: &str) -> PathBuf`.
  - `config.rs`:
    - `pub enum Schedule { Every { seconds: u64 }, At { times: Vec<(u8, u8)> }, Manual }`
    - `pub struct QuietWindow { pub start: u16, pub end: u16 }` (minutes after midnight)
    - `pub struct Job { pub id: String, pub description: String, pub command: String, pub cwd: PathBuf, pub schedule: Schedule, pub quiet: Option<QuietWindow>, pub timeout_sec: u64, pub url: Option<String> }`
    - `pub struct ConfigError(pub String)` implementing `Display` and `Error`.
    - Functions: `parse_duration(&str) -> Result<u64, ConfigError>`, `parse_quiet(&str) -> Result<QuietWindow, ConfigError>`, `parse_config(&str) -> Result<Vec<Job>, ConfigError>`, `load_config(&Path) -> Result<Vec<Job>, ConfigError>`.
  - `schedule.rs`:
    - `in_quiet(Option<&QuietWindow>, DateTime<Local>) -> bool`
    - `next_run(&Job, last_start: Option<DateTime<Local>>, now: DateTime<Local>) -> Option<DateTime<Local>>`
    - `format_countdown(ms: i64) -> String`
    - `describe_schedule(&Schedule) -> String`

- [ ] **Step 1: Create the worktree.** Run `git worktree add -b rust-rewrite ../homelab-hq-rust main`, then `cd ../homelab-hq-rust && cargo init --name hq`, and add the dependencies from Tech Stack.
- [ ] **Step 2: Write the failing tests.** Port every case in `test/config.test.ts` (16) and `test/schedule.test.ts` (17) one-to-one. Keep the test names in snake_case and the exact message strings, e.g. `"jobs.<id>: unknown key colour"`, `"every must be at least 60s"`, `"url must start with http:// or https://"`. Also port the parts of `test/stacks.test.ts` "job url" and "jobs config accepts a stacks table and ignores it".
- [ ] **Step 3: Run `cargo test`.** Expected: FAIL to compile, because the functions are missing.
- [ ] **Step 4: Implement `paths.rs`, `config.rs` and `schedule.rs`.**
  - `parse_config` accepts the top-level keys `jobs` and `stacks` (it ignores `stacks`) and sorts manual jobs first, then by id.
  - Parse the TOML into `toml::Table` and walk it by hand. Don't use derive: the error messages must match the TS strings, and unknown keys must be rejected by name.
- [ ] **Step 5: Run `cargo test`.** Expected: all ported cases pass.
- [ ] **Step 6: Commit.** `git commit -am "rust: scaffold, paths, config, schedule"`

### Task 2: History, state, log

**Files:**
- Create: `src/history.rs`, `src/state.rs`, `src/log.rs`

**Interfaces:**
- Produces:
  - `history.rs`:
    - `pub enum Status { Running, Ok, Failed, Timeout, Skipped }` and `pub enum Trigger { Scheduled, Manual }`, both with `as_str`/`parse` using the TS lowercase strings.
    - `pub struct RunRow { id: i64, job: String, trigger: Trigger, started_at: i64, ended_at: Option<i64>, exit_code: Option<i32>, status: Status, reason: Option<String>, tail: Option<String> }`. Times are epoch ms.
    - `History::open(&Path) -> rusqlite::Result<History>`
    - Methods: `start(job, trigger, started_at) -> i64`, `finish(id, ended_at, exit_code, status, tail)`, `fail_stale(job, at, reason) -> usize`, `skip(job, trigger, reason, at) -> i64`, `recent(job, limit) -> Vec<RunRow>`, `last_real(job) -> Option<RunRow>`, `prune(now, max_age_ms) -> usize`.
  - `state.rs`: `read_paused(&Paths) -> BTreeSet<String>` and `set_paused(&Paths, id, bool) -> io::Result<()>`.
  - `log.rs`: `append_log(&Paths, id, &[u8]) -> io::Result<()>`, `rotate_if_needed(&Paths, id, max_bytes: u64)` and `TailBuffer::new(max) / push(&[u8]) / text() -> String`. `TailBuffer` decodes lossily.

- [ ] **Step 1: Write the failing tests.**
  - Port `test/history.test.ts` (6), `test/state.test.ts` (2) and the TailBuffer and rotation cases from `test/runner.test.ts`.
  - Add `opens_db_created_by_ts_schema`:
    1. Create a DB using the TS `SCHEMA` text verbatim.
    2. Insert a finished row and a skipped row with `NULL` tail.
    3. Open it with `History::open`.
    4. Assert that `recent` returns both rows, and `last_real` returns the finished one.
  - Add `state_json_format`: `set_paused(a)` followed by `set_paused(b)` writes exactly `{\n  "paused": [\n    "a",\n    "b"\n  ]\n}`.
- [ ] **Step 2: Run `cargo test`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
  - Run the SQL with `PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;` before `CREATE TABLE IF NOT EXISTS`. Use the same statements as `src/history.ts`.
  - `failStale` uses `COALESCE(tail || char(10), '') || ?`.
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "rust: history, state, log"`

### Task 3: launchd

**Files:**
- Create: `src/launchd.rs`

**Interfaces:**
- Consumes: `Paths`, `Job`, `read_paused`/`set_paused`.
- Produces:
  - `pub struct CmdOutput { pub code: i32, pub stdout: String, pub stderr: String }`
  - `pub trait Launchctl { fn exec(&self, args: &[&str]) -> CmdOutput; }` with two implementations:
    - `RealLaunchctl`
    - `FakeLaunchctl { paths: Paths }`. It appends `args.join(" ")` to `paths.launchctl_calls()`. For `print`, it returns code 0 with `"\tstate = not running\n"` if the plist exists, otherwise code 113 with `"Could not find service"`. This matches `src/exec.ts`.
  - `pub fn launchctl_from_env(paths: &Paths) -> Box<dyn Launchctl>` (honours `HQ_FAKE_LAUNCHCTL`).
  - `pub struct PlistCtx { pub bin: PathBuf, pub hq_home: Option<PathBuf> }` and `PlistCtx::current(&Paths)`, where `bin` comes from `std::env::current_exe()`.
  - `render_plist(&Job, &PlistCtx, &Paths) -> String`, which errors (panics in debug) on manual jobs, as the TS does.
  - `pub struct AgentState { pub linked: bool, pub running: bool, pub last_exit: Option<i32> }` and `agent_state(id, &dyn Launchctl) -> AgentState`.
  - `pub struct SyncPlan { pub add: Vec<String>, pub change: Vec<String>, pub remove: Vec<String> }`
  - Functions: `plan_sync(&[Job], &PlistCtx, &Paths) -> io::Result<SyncPlan>`, `apply_sync(&SyncPlan, &[Job], &PlistCtx, &Paths, &dyn Launchctl) -> Result<(), String>`, `hold(id, &Paths, &dyn Launchctl) -> io::Result<()>`, `resume(&Job, &PlistCtx, &Paths, &dyn Launchctl) -> Result<(), String>`, `uninstall(&Paths, &dyn Launchctl) -> Result<(), String>`. Their semantics are the same as `src/launchd.ts`.

- [ ] **Step 1: Write the failing tests.**
  - Port `test/launchd.test.ts` (12). In the plist golden tests, replace `[bunPath, cliPath, "run", id]` with `[bin, "run", id]`; everything else in the expected XML stays byte-identical.
  - Add `plist_escapes_xml`: a bin path containing `&` and `<` is rendered escaped.
- [ ] **Step 2: Run `cargo test launchd`.** Expected: FAIL.
- [ ] **Step 3: Implement `src/launchd.rs`.** `agent_state` reads only top-level keys: lines starting with exactly one tab, `\tstate = ` and `\tlast exit code = `.
- [ ] **Step 4: Run `cargo test launchd`.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "rust: launchd"`

### Task 4: Runner

**Files:**
- Create: `src/runner.rs`

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces:
  - `pub const TIMEOUT_EXIT: i32 = 124;`
  - `pub struct RunOpts { pub manual: bool, pub kill_grace: Duration, pub now: fn() -> DateTime<Local> }`
  - `run_job(&Job, &Paths, &History, &RunOpts) -> i32`, which never panics: every path ends in `history.finish` and an END line.
  - `job_env() -> Vec<(String, String)>`
  - `format_duration(ms: i64) -> String`

- [ ] **Step 1: Write the failing tests.**
  - Port `test/runner.test.ts` (16). This covers skip on hold, skip in quiet, skip on overlap, stale-run cleanup, timeout → 124 with SIGTERM then SIGKILL, a forwarded SIGTERM recorded as failed, the drain, a missing cwd, and the log framing.
  - Add `non_utf8_and_split_multibyte_output`: the command is `printf '\xff\xe2\x94'; sleep 0.1; printf '\x80 done'`. The log bytes equal the raw output exactly. The tail is `"\u{FFFD}── done"` or another lossy-but-valid UTF-8 string ending in `done`. There's no panic.
- [ ] **Step 2: Run `cargo test runner`.** Expected: FAIL.
- [ ] **Step 3: Implement `run_job`.**
  - Spawn with `Command::new("/bin/zsh").args(["-lc", cmd]).process_group(0)`, with stdout and stderr piped.
  - Two reader threads and one `child.wait()` thread send `Event::{Out(Vec<u8>), Eof, Exit(ExitStatus)}` on one `mpsc` channel.
  - The main thread uses `recv_timeout` against the job deadline. On timeout it calls `libc::killpg(pgid, SIGTERM)`, then SIGKILL after `kill_grace`.
  - After `Exit`, wait at most 2 s for both `Eof`s.
  - Signal forwarding: register a `signal_hook::flag` for SIGTERM, SIGINT and SIGHUP, and check it in the receive loop at 100 ms.
  - The lock uses `OpenOptions::new().write(true).create_new(true)`. A stale lock is detected with `libc::kill(pid, 0)`, where `EPERM` means the holder is alive.
- [ ] **Step 4: Run `cargo test runner`.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "rust: runner"`

### Task 5: Snapshot and CLI

**Files:**
- Create: `src/snapshot.rs`, `tests/cli.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - `pub struct JobView { job, held, agent: AgentState, synced, last: Option<RunRow>, recent: Vec<RunRow>, running, next: Option<DateTime<Local>> }`
  - `pub struct Snapshot { jobs: Vec<JobView>, config_error: Option<String>, at: DateTime<Local> }`
  - `load_snapshot(&Paths, &History, agents: &HashMap<String, AgentState>, now, previous: Option<&Snapshot>) -> Snapshot`. It takes the agent states as an input: the CLI fetches them fresh, and the TUI fetches them on its own schedule.
  - `fetch_agents(&[Job], &dyn Launchctl) -> HashMap<String, AgentState>`
  - `summarize(&Snapshot) -> Summary { systems, nominal, fault, standby }`
  - CLI commands, with the same output and usage text as `src/cli.ts`: `status [--json]`, `run <job> [--manual]`, `logs <job> [-f]`, `hold`, `resume`, `sync [--yes]`, `uninstall`. With no command it opens the TUI, which until Task 8 just prints `tui: not yet`.

- [ ] **Step 1: Write the failing tests.**
  - Port `test/cli.test.ts` (10) to `tests/cli.rs`. Run `env!("CARGO_BIN_EXE_hq")` with `HQ_HOME`, `HQ_CONFIG`, `HQ_LAUNCH_AGENTS_DIR` and `HQ_MIGRATED_DIR` pointing at a tempdir, and `HQ_FAKE_LAUNCHCTL=1`.
  - Keep the `run` config-error case: it records a failed run and a log line `hq: config error: …`.
  - `status --json` emits the same field names as TS (`job`, `held`, `agent`, `synced`, `last`, `recent`, `running`, `next`), with camelCase inside the rows (`startedAt` and so on).
- [ ] **Step 2: Run `cargo test --test cli`.** Expected: FAIL.
- [ ] **Step 3: Implement `snapshot.rs` and the dispatch in `main.rs`.** `logs -f` polls the file size every 500 ms and reprints from the start when the file has rotated.
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Run the read-only parity check on real data.** Run `cargo run --release -- status` and `bun src/cli.ts status` in the live checkout. Expected: identical lines.
- [ ] **Step 6: Commit.** `git commit -am "rust: snapshot and CLI"`

### Task 6: Stacks and docker

**Files:**
- Create: `src/stacks.rs`

**Interfaces:**
- Produces:
  - `pub struct Stack { name, dir: PathBuf, files: Vec<PathBuf>, env_file: Option<PathBuf> }`
  - `parse_stacks(&str) -> Result<Vec<Stack>, ConfigError>` and `load_stacks(&Path)`.
  - `pub trait Docker: Send + Sync { fn exec(&self, args: &[String], timeout: Option<Duration>) -> Result<CmdOutput, String>; }` and `RealDocker`. `RealDocker` uses a 15 s timeout for polls; actions pass `None`.
  - `compose_args(&Stack, rest: &[&str]) -> Vec<String>`
  - `pub enum ServiceState { Running, Restarting, Paused, Exited, Created, Dead, Missing }`
  - `pub struct ServiceView { stack, service, container: Option<String>, image, ports: Vec<u16>, state, health, status, exit_code: i32, cpu: Option<f64>, mem: Option<u64> }`
  - `pub struct StackView { stack: Stack, services: Vec<ServiceView>, error: Option<String> }`
  - `pub struct ConfigCache` (keyed by file mtimes)
  - Functions:
    - `load_stack_view(&Stack, &dyn Docker, &HashMap<String, Usage>, &mut ConfigCache) -> StackView`
    - `load_usage(&dyn Docker) -> Result<HashMap<String, Usage>, String>`
    - `parse_bytes`, `parse_stats`, `parse_compose_config`
    - `is_up`, `is_service_fault`, `service_url`, `totals`, `format_bytes`, `short_uptime`

- [ ] **Step 1: Write the failing tests.** Port the stack, docker-parsing, `loadStackView` and `shortUptime` cases from `test/stacks.test.ts`. Add `ps_output_as_json_array_or_ndjson`: both formats parse to the same services.
- [ ] **Step 2: Run `cargo test stacks`.** Expected: FAIL.
- [ ] **Step 3: Implement.** A missing `docker` binary maps to `Err("docker not found")`.
- [ ] **Step 4: Run `cargo test stacks`.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "rust: stacks"`

### Task 7: TUI core: event loop, theme, Effects

**Files:**
- Create: `src/tui/mod.rs`, `src/tui/theme.rs`

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces:
  - `theme.rs`:
    - Colour constants `BODY`, `ACCENT`, `FAULT`, `HOLD` (as `Color::Rgb`) and the width constants `MAX_WIDTH`, `MARGIN` and `INDENT`.
    - `inner_width(cols: u16) -> u16`
    - `clamp_lines(text, width, max) -> Vec<String>` (same results as the TS)
    - `truncate_width(&str, width) -> String`, which adds `…` when it cuts
    - `glyph(&JobView) -> &'static str`, `is_fault`, `clock_line(DateTime<Local>) -> String`, `hhmm`, `format_seconds`, `minutes_to_clock`
  - `mod.rs`:
    - `pub enum View { Jobs, Services }` and `pub enum Mode { Dash, Help, Log(String), Sync(Result<SyncPlan, String>), Stack { stack: String, start: bool } }`
    - `pub trait Effects { fn spawn_cli(&self, args: &[&str]); fn open(&self, url: &str); fn docker(&self) -> Arc<dyn Docker>; }`
    - `pub struct App { ... }` with `App::handle_key(&mut self, KeyEvent, &dyn Effects) -> bool` (returns `false` to quit) and `App::apply(&mut self, Msg)`
    - `pub enum Msg { Snapshot(Snapshot), Agents(HashMap<..>), Stacks(Result<Vec<StackView>, String>), Usage(HashMap<..>), ActionFailed(String) }`
    - `pub fn run(paths: Paths) -> io::Result<()>`

- [ ] **Step 1: Write the failing tests.** Port the theme-level cases (`clampLines`, `clockLine`, `formatSeconds`) from the dashboard and TS tests. Add `truncate_width_counts_display_width`: `truncate_width("ab──😀cd", 5) == "ab──…"`.
- [ ] **Step 2: Run `cargo test tui::theme`.** Expected: FAIL.
- [ ] **Step 3: Implement the theme and the event loop.**
  - `crossterm::event::poll(timeout)` waits until the next whole second.
  - On each wake:
    1. Drain the `Msg` channel.
    2. `stat` the config, `history.db-wal`, `history.db`, `state.json` and the agents dir. If any mtime changed, reload the snapshot on the main thread, using the cached agents.
    3. Redraw only if something changed or the second rolled over.
  - Worker threads:
    - Agents: every 30 s, and immediately after S, P or R.
    - Stacks: every 3 s in the services view, every 15 s in the jobs view.
    - Usage: every 6 s / 30 s on the same split.
  - Each worker sleeps on a `Condvar`, so a view switch can wake it.
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "rust: tui core"`

### Task 8: Jobs view and help

**Files:**
- Create: `src/tui/jobs.rs`, `src/tui/help.rs`

**Interfaces:**
- Produces: `draw_jobs(&mut Frame, &App)` and `draw_help(&mut Frame, View)`. Copy the strings from `Dashboard.tsx`, `JobRow.tsx`, `DetailPanel.tsx` and `HelpView.tsx` verbatim, including the `KEYS` line and the `SERVICES … Tab ▸` line.

- [ ] **Step 1: Write the failing tests.**
  - Port `test/tui/dashboard.test.tsx` (10) and the jobs and help cases of `test/tui/app.test.tsx` (9). Render to `TestBackend::new(100, 40)` and assert on `buffer` lines. Build the fixtures from the TS `test/fixtures/snapshot.ts`.
  - Key handling (arrows, ⏎, R, P, S + Y, O, H, Q) is asserted through `handle_key` with a recording fake `Effects`.
  - Add `renders_tiny_terminal_without_panic`: jobs, help, log and services, each at 20×5.
  - Add `row_with_emoji_description_fits_width`: no buffer line is wider than `inner_width + MARGIN`.
- [ ] **Step 2: Run `cargo test tui::jobs`.** Expected: FAIL.
- [ ] **Step 3: Implement the two draw functions.**
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "rust: jobs view and help"`

### Task 9: Log view

**Files:**
- Create: `src/tui/logview.rs`

**Interfaces:**
- Produces:
  - `pub struct LogState { lines: Vec<String>, cursor: Option<usize>, expanded: HashSet<usize>, wrap_all: bool, top: usize, len_seen: u64 }`
  - Pure helpers: `chunk(&str, width) -> Vec<String>`, `rows_for(&str, width, expanded) -> Vec<String>` and `view_top(heights: &[usize], cursor, height, start, anchor_bottom: bool) -> usize`.
  - `draw_log(&mut Frame, &LogState, id)` and `LogState::handle_key(KeyEvent)`.
  - Following reads only appended bytes (from `len_seen`) and re-reads in full if the file shrank (it rotated).

- [ ] **Step 1: Write the failing tests.** Port `test/tui/logview.test.tsx` (10): chunk, rowsFor, viewTop, opens LIVE, ⏎ expand and collapse, w and ↓ back to LIVE. Add `truncates_wide_chars_by_width`: a 200-column line of `─` and emoji, with the cursor on it, renders within the width and ends with `…`.
- [ ] **Step 2: Run `cargo test tui::logview`.** Expected: FAIL.
- [ ] **Step 3: Implement.** `chunk` splits by display width, not by characters, so that it agrees with `truncate_width`.
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "rust: log view"`

### Task 10: Services view

**Files:**
- Create: `src/tui/services.rs`

**Interfaces:**
- Consumes: `StackView`, `Effects::docker`.
- Produces: `draw_services(&mut Frame, &App)`, the services key handling inside `App::handle_key` (↑↓, O, X, Z + Y, Tab) and the pending map `HashMap<String, (Pending, Instant)>` (dropped after 120 s).

- [ ] **Step 1: Write the failing tests.**
  - Port `test/tui/services.test.tsx` (6). Use the same fake docker: `stop`/`up` change the running set; actions get `timeout == None` and polls get `Some(15 s)`. Keep the same strings, e.g. `stop 2 services in LAB?  Y confirm · any key cancel`, `ERROR — lab: up -d radarr — Error: drive not mounted`.
  - Add `docker_missing_shows_error_tui_stays_up`: the fake docker returns `Err("docker not found")`. The services view shows `ERROR — docker not found`, and Tab, H and Q still work.
- [ ] **Step 2: Run `cargo test tui::services`.** Expected: FAIL.
- [ ] **Step 3: Implement.** Actions run on a spawned thread and report `Msg::ActionFailed`. A successful action wakes the stacks worker.
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Commit.** `git commit -am "rust: services view"`

### Task 11: Performance verification and screenshots

**Files:**
- Create: `examples/screenshots.rs`, a port of `tools/screenshots.tsx`. It renders each view with the demo data from `tools/demo.ts` into a `TestBackend` and converts the buffer cells to the same HTML, then to PNG with headless Chrome.
- Modify: `docs/images/*.png`

- [ ] **Step 1: Build.** Run `cargo build --release`. Check the binary size with `ls -la target/release/hq` and record the number.
- [ ] **Step 2: Measure** with a temp `HQ_HOME`, `HQ_FAKE_LAUNCHCTL=1` and the real stacks config, as in the 2026-10-18 measurement. Sample RSS and CPU time over 30 s for: jobs view without stacks, jobs view with stacks, and services view. Also measure `/usr/bin/time -l target/release/hq status`. Expected: the Global Constraints targets are met. Record the numbers in the commit message.
- [ ] **Step 3: Generate the screenshots.** Run `cargo run --example screenshots`. Expected: `dashboard`, `dashboard-fault`, `help`, `log` and `services` PNGs that look like the current ones. Check them visually.
- [ ] **Step 4: Commit.** `git commit -am "rust: screenshots; perf: <numbers>"`

### Task 12: Cutover, docs and TS removal

**Files:**
- Modify: `README.md`, `CLAUDE.md`, `skills/hq-add-job/SKILL.md`
  - Replace bun commands with cargo ones.
  - Change the install step to `cargo install --path . --root ~/.local`.
  - Update the Development and Architecture sections for the Rust modules.
  - Update the test count.
- Delete: `src/**/*.ts(x)`, `test/`, `tools/`, `package.json`, `bun.lock`, `tsconfig.json`, `node_modules/`

- [ ] **Step 1: Tag the rollback point** in the live checkout: `git tag ts-final main`.
- [ ] **Step 2: Install** from the worktree: `cargo install --path . --root ~/.local`. Expected: `~/.local/bin/hq --help`-style usage is printed with exit 2. `~/.local/bin/hq status` matches the TS output.
- [ ] **Step 3: STOP and ask the owner before switching the LaunchAgents.** After approval, run `~/.local/bin/hq sync`. The plan should list all 4 scheduled jobs as `~` (changed), because the program path changed. Then run it with `--yes`.
- [ ] **Step 4: Verify the live switch.**
  - `plutil -p ~/Library/LaunchAgents/com.chngu.hq.trml-reminders.plist` shows `ProgramArguments` starting with `~/.local/bin/hq`.
  - Within 5 minutes, `~/.local/bin/hq status` shows a new `trml-reminders` run marked OK, and its log has a fresh RUN/END block.
- [ ] **Step 5: Remove the Bun link** so `~/.bun/bin/hq` no longer shadows the new binary on PATH: `bun remove -g homelab-hq`. Expected: `which hq` returns `~/.local/bin/hq`.
- [ ] **Step 6: Delete the TS sources and update the docs.** Then run `cargo test`. Expected: PASS.
- [ ] **Step 7: Merge.** Commit with `git commit -am "rust: cutover; remove TypeScript"`. In the live checkout, run `git merge --ff-only rust-rewrite`, then `git worktree remove ../homelab-hq-rust`.
- [ ] **Step 8: Document the rollback in `CLAUDE.md`:**
  1. `git checkout ts-final`
  2. `bun install && bun link`
  3. `bun src/cli.ts sync --yes`
  4. `rm ~/.local/bin/hq`
