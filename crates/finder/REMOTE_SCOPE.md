# Scope: Remote-aware command execution in the Finder

## Glossary (domain model)

- **Source** — a Finder config entry whose command produces the list of entries
  (`rg`, `git branch`, …). Implemented by `SourceRunner::spawn` →
  `DefaultSourceRunner`, which today spawns a *local* `smol` process.
- **Outcome** — what happens when the user confirms an entry. `RunCommand`
  outcomes spawn a process via `CommandRunner::run` → `DefaultCommandRunner`,
  also *local* today.
- **Runner singleton** — both runners are resolved through a GPUI global
  (`source_runner(cx)` / `command_runner(cx)`) with a `set_*_runner` override
  used **only** by tests. The runner has **no access to the `Project` entity**;
  its signature is `(command, args, cwd, env)`.
- **cwd** — the first visible worktree's `abs_path()`. For a remote project this
  is a **remote path** that does not exist on the local host.
- **`Project::is_remote()`** — `!is_local()`; true for collab *and*
  remote-server projects. The Finder crate contains **zero** references to
  `remote`/`is_remote`.
- **`Project::remote_client()` → `Option<Entity<RemoteClient>>`** — the handle
  to the remote transport (SSH / WSL / Docker). `None` for local projects, but
  also `Some` for WSL-with-interop where `is_via_wsl_with_host_interop` is true
  even though `client_state` is `Local`.
- **`RemoteClient::build_command(program, args, env, working_dir, port_forward, interactive) -> Result<CommandTemplate>`**
  — the transport-aware command resolver. For SSH it returns
  `CommandTemplate { program: "ssh", args: [opts, -T, destination, "cd <dir> ; exec env K=V … <program> <args>"], env: ssh_env }`.
  The **project env is baked into the ssh arg string**; the template's own
  `env` is the SSH *connection* env (`ssh_env`), not the project env.
- **`CommandTemplate { program, args, env }`** — a fully-resolved, locally-
  spawnable command. The transport decides the `program` (`ssh`, the WSL
  launcher, `docker exec`, or `"mock"` in tests).
- **Local branch** — the path the Finder takes today: `new_command(command)`
  with `.current_dir(cwd)` and `.envs(project_env)`.
- **Remote branch** — the path `Project::exec_in_shell` takes: build a
  `CommandTemplate` via `remote_client.build_command(...)`, then spawn
  `template.program`/`template.args`/`template.env` with **no** local
  `current_dir` (the `cd` is inside the ssh args).
- **`RunOn`** — a per-finder execution policy (`auto` | `local`, default `auto`),
  declared as a top-level `run_on = "…"` key in the finder TOML. `auto` follows
  the project (remote when `remote_client()` is present, else local) — this is the
  bug fix. `local` forces the command to run on this machine even when the
  project is remote, an escape hatch for finders that must query the local host.
  Whole-finder: it governs both the Source spawn and the `RunCommand` outcome.
  `OpenPath`/`DispatchAction` outcomes ignore it (they never spawn a process).

## Grilling: interrogating the scope

Each question is answered with a consequence for the implementation.

### G1. Where should the local-vs-remote decision live?

**Claim:** at the *call site*, not inside the runner.

The `SourceRunner`/`CommandRunner` traits are deliberately transport-agnostic
seams (they take already-resolved `command/args/cwd/env`). The `Project` entity —
the only thing that knows whether the project is remote and holds the
`RemoteClient` — is available at the call sites (`spawn_source`,
`update_query_matches`, `apply_outcome`) but **not** inside the runners. So the
transport resolution must happen where the `Project` is, exactly as
`exec_in_shell` does. Keeping the runner dumb preserves the existing test
injection and avoids widening the trait.

**Consequence:** add a helper `resolve_command(project, command, args, cwd, env,
cx) -> (program, args, env, cwd_for_local_spawn)`. Call it in three places. The
runners stay as local spawners — they just receive a `program` that is now
`"ssh"` when remote.

### G2. Does the runner trait / `run_source` signature change?

**Yes, minimally, and only for `cwd`.**

`run_source` takes `cwd: Arc<Path>` and forwards it to
`SourceRunner::spawn`, which calls `.current_dir(cwd)`. For a remote command the
`cd` is embedded in the ssh args, so the **local** spawn must **not** set
`current_dir` to the remote path (it doesn't exist locally → spawn fails).
`exec_in_shell`'s remote branch simply omits `current_dir`.

So `run_source`'s `cwd` must become `Option<Arc<Path>>`: `Some` on the local
branch, `None` on the remote branch. The trait's `spawn(.., cwd: &Path, ..)`
becomes `Option<&Path>`. This ripples to **three** impls
(`DefaultSourceRunner`, `ScriptedRunner`, the `run_source` body) — small, and
`ScriptedRunner` already ignores `cwd`.

**Consequence:** one trait signature tweak + its three impls. `CommandRunner` in
`outcome.rs` already owns its own `DefaultCommandRunner::run` body and is not a
shared trait implementation across crates, so its `cwd` handling is local to that
file.

### G3. What happens to the environment the Finder already fetches?

**It must be *replaced*, not merged, on the remote branch.**

Today `spawn_source` calls `worktree_environment` → for remote, that RPCs the
remote server and returns the **remote** env. That env is currently handed to a
*local* spawn (wasted — the bug). After the fix, on the remote branch that env
is passed as `input_env` to `build_command`, which **bakes it into the ssh arg
string** (`exec env K=V …`). The `CommandTemplate.env` returned is then the
**ssh connection env** (`ssh_env`), and **that** is what the local `ssh`
process must run with. Using the project env for the local `ssh` spawn would
leak local/remote PATH confusion back in.

So the call site must, on the remote branch, **discard** the fetched project env
for the spawn and use `template.env` instead. (The project env is not wasted —
it rode into the ssh args.)

**Consequence:** the helper returns the *spawn* env, which differs by branch.
The existing `worktree_environment` fetch is still needed (it feeds
`build_command`), but its result is not the env passed to `spawn`.

### G4. Is `cwd` for `build_command` the raw `abs_path` or something massaged?

`build_command` takes `working_dir: Option<String>` and each transport handles
quoting/tilde itself (`build_command_posix` does `cd <quoted>` / `cd "$HOME"/…`
for `~/`). So pass the remote `abs_path` as-is. `None` only if there is no
worktree — but `open_finder` already bails when `cwd` is `None`, so `cwd` is
always `Some` at these call sites.

**Consequence:** no path massaging needed; pass `cwd.to_string_lossy()`.

### G5. WSL-with-host-interop: is the project "remote"?

This is the sharpest edge. `is_via_wsl_with_host_interop` is true when
`client_state` is `Local`/`Shared` **but** `remote_client` is `Some` and has WSL
interop. `Project::is_remote()` returns **false** here (it's `!is_local()` and
`client_state` is `Local`). So gating on `is_remote()` alone would **miss WSL**
and keep running `rg` on the Windows host.

The correct gate is **`project.remote_client().is_some()`**, not `is_remote()`.
That matches `exec_in_shell`, which branches on `remote_client` presence, not
`is_remote`. `build_command` is the same method for SSH, WSL, and Docker
transports.

**Consequence:** branch on `remote_client().is_some()`, never on `is_remote()`.
This single choice makes WSL/Docker/SSH all work and is the most important
scoping decision.

### G5b. What does `run_on = "local"` do to `cwd` on a remote project?

This is the sharpest override edge. The Source/outcome always pass the
worktree's `abs_path()` as `cwd`. On a remote project that is a **remote**
path. Forcing `local` would hand that remote path to a *local*
`current_dir()` and re-fail the original way. So the resolver must **drop** `cwd`
(`None`) whenever it spawns locally on a remote project: the command runs in
the Zed process's cwd, and the user — who explicitly asked for local execution
— owns making the command work without the remote project as its working
directory. Concretely the local-branch `cwd` is `Some(cwd)` only when the
project is local; `None` when forcing `local` on a remote project.

**Consequence:** the resolver branches on `(run_on, remote_client.is_some())`:
`(Auto, remote)` → remote branch; `(Auto, local)` / `(Local, local)` → local
with `Some(cwd)`; `(Local, remote)` → local with `cwd = None`.

### G6. What if `build_command` returns `Err` (no remote connection)?

`RemoteClient::build_command` returns `Err("no remote connection")` when
`remote_connection()` is `None` (e.g. connection dropped mid-session). The
helper must surface this as a `SourceFailure::CouldNotSpawn` (source) /
`show_error` (outcome) rather than panic or silently fall back to local
(spawning `rg` locally against a remote path is the original bug).

**Consequence:** helper returns `Result`; `Err` becomes a spawn failure, not a
local fallback.

### G7. Do the existing 68 tests break?

No. Tests build a **local** `Project::test` → `remote_client()` is `None` → the
local branch is taken → behaviour is identical to today. The `Option<&Path>`
cwd change touches `ScriptedRunner` (which ignores cwd) and `run_source`'s body.

**Consequence:** zero behavioural change for existing tests; only signature
adjustments in `ScriptedRunner::spawn` and `run_source` call sites.

### G8. How is the *remote* path tested?

The mock transport (`crates/remote/src/transport/mock.rs`) `build_command`
returns `CommandTemplate { program: "mock", args: [shell_program, …input_args],
env: input_env }`. `RemoteClient::fake_server`/`fake_client` build a mock remote
client. So a remote-finder test constructs a `Project::remote` with the mock
client and asserts the source runner is invoked with `program = "mock"` and the
original args wrapped — i.e. that `rg` was **not** spawned directly.

Caveat: `ScriptedRunner` currently captures `spawned_args` but **not** the
command/program. To assert the remote transform happened, add `command` capture
to `ScriptedRunner` (a ~3-line test-support addition). The mock transport gives
a deterministic `program="mock"` signal.

**Consequence:** one test-support tweak (capture command) + new remote tests
using the mock transport. No new production seam needed — the mock transport
*is* the seam.

### G9. Is the preview (`OpenPath`/`OpenPathAtPosition`) affected?

No. Previews open paths through the `Project`/`Fs`, which is already
remote-aware (paths resolve against remote worktrees). Only the *command-
spawning* outcomes (`Source`, `RunCommand`) are local-broken. `DispatchAction`
runs a GPUI action locally, which is correct everywhere.

**Consequence:** scope is exactly the two command runners, nothing else.

### G10. `Interactive::Yes` or `No`?

`No`. Sources and `RunCommand` communicate via piped stdio, not a TTY.
`exec_in_shell` uses `Yes` because it spawns an interactive terminal. The mock
transport ignores `interactive`; SSH uses it to pick `-T` vs `-t`.

**Consequence:** pass `Interactive::No` to `build_command`.

### G11. Does `env_clear()` in `DefaultCommandRunner` clash?

`DefaultCommandRunner::run` calls `.env_clear().envs(environment)`. On the
remote branch the `environment` it receives will be the **template env**
(ssh_env), and `.env_clear()` + `.envs(ssh_env)` is correct for the local `ssh`
spawn. So the existing `env_clear` is fine **provided** the helper swaps the env
to the template env on the remote branch (G3). No change to the runner body
beyond what the cwd-Option tweak requires.

**Consequence:** no extra change; G3's env swap covers it.

## Decision (ADR — see docs/adr/0005)

> **Finder commands resolve through the project transport before spawn, with
> a per-finder `run_on` override.** The call site (`spawn_source`,
> `update_query_matches`, `apply_outcome`) consults `project.remote_client()`
> *and* the finder's `run_on` policy. Under `auto` (default), when a remote
> client is present it builds a `CommandTemplate` via
> `RemoteClient::build_command(.., Interactive::No)` and spawns
> `template.program`/`args`/`env` with **no** local `current_dir`; otherwise it
> keeps today's local spawn. Under `local`, it always spawns locally, dropping
> `cwd` (`None`) when the project is remote so the remote path is never handed
> to a local `current_dir`. The runner traits stay transport-agnostic; only
> `cwd` becomes `Option` to express "cd is baked into the args, or omitted".

Gate on `remote_client().is_some()`, **not** `is_remote()`, so WSL/Docker/SSH
are all covered with one branch. `auto` is the default, so existing configs get
the remote fix automatically; `local` is the escape hatch for finders that must
query the local host.

## Implementation scope (sized)

### Layer 0 — config: `RunOn` policy (new, ~25 lines + tests)
- `pub enum RunOn { Auto, Local }` in `config.rs`, `#[serde(rename_all =
  "lowercase")]`, `impl Default = Auto`.
- Add `run_on: RunOn` to `FinderBody` (with `#[serde(default)]`, so existing
  configs parse unchanged) and to `FinderConfig`; thread it through
  `try_into_config`/`into_config`.
- Tests: `run_on = "local"` parses to `RunOn::Local`; omitting it defaults to
  `Auto`; an unknown value (`run_on = "mars"`) lands the finder in
  `ParsedConfig::errors` (serde rejects unknown enum variants).

### Layer 1 — transport resolution helper (new, ~70 lines)
- `resolve_command_for_project(project, run_on, command, args, cwd, env, cx) -> Result<(String, Vec<String>, HashMap<String,String>, Option<Arc<Path>>)>`
- Lives in a new `crates/finder/src/remote.rs` (or in `source.rs`/`outcome.rs`).
- Branch on `(run_on, project.remote_client().is_some())`:
  - `(Auto, remote)` → `build_command(.., Interactive::No)`, return
    `(template.program, template.args, template.env, None)`.
  - `(Auto, local)` / `(Local, local)` → local, return
    `(command, args, env, Some(cwd))`.
  - `(Local, remote)` → forced-local-on-remote, return
    `(command, args, env, None)` (cwd dropped, per G5b).

### Layer 2 — wire the helper into three call sites (~20 lines)
- `FinderDelegate::spawn_source` and `update_query_matches` (delegate.rs),
  passing `self.config.run_on`.
- `apply_outcome` `RunCommand` branch (outcome.rs), passing the finder's
  `run_on` (thread it into `apply_outcome`'s signature, or read it from the
  `Outcome`/config the caller already holds).
- Replace the fetched project env with the helper's spawn env on the remote
  branch (G3).

### Layer 3 — `cwd` → `Option` (~15 lines + test tweaks)
- `SourceRunner::spawn(.., cwd: &Path, ..)` → `Option<&Path>`.
- `DefaultSourceRunner`, `ScriptedRunner`, `run_source` body: handle `None`.
- `DefaultCommandRunner::run`: same `Option` treatment for its `current_dir`.

### Layer 4 — tests (~150 lines)
- Existing 68 tests: signature-only adjustments (pass `Some(cwd)` /
  `Some(Path::new("/project"))`, default `run_on = Auto`). No behavioural
  change.
- `ScriptedRunner`: capture `command` (3 lines) so remote tests can assert the
  transform.
- New remote tests: build a mock remote project (`RemoteClient::fake_server` +
  `Project::remote`), drive a Source and a `RunCommand` outcome, assert the
  runner sees `program = "mock"` and the original command wrapped — i.e. `rg` is
  **not** spawned directly against a remote cwd.
- Override tests: same mock remote project with `run_on = "local"` → the runner
  sees the raw `rg` (not `"mock"`) and `cwd = None` (no `current_dir`), proving
  the escape hatch bypasses the transport.
- Regression test for G6: `build_command` `Err` → `CouldNotSpawn`, not local
  fallback.
- Config tests (Layer 0): `run_on` parses/defaults/rejects.

### Out of scope
- Preview path opening (already remote-aware).
- `DispatchAction` (local by design).
- Any change to `RemoteClient`/transports (we only *consume* `build_command`).
- Caching of the resolved command.

## Risks

| # | Risk | Mitigation |
|---|------|-----------|
| R1 | Gating on `is_remote()` misses WSL | Gate on `remote_client().is_some()` (G5) |
| R2 | Passing remote env to the local `ssh` spawn re-introduces PATH confusion | Use `template.env`, not project env, on remote branch (G3) |
| R3 | Setting `current_dir(remote_path)` on the local `ssh` spawn fails | `cwd` becomes `Option`; `None` on remote branch (G2) |
| R4 | `build_command` `Err` during a dropped connection silently runs locally | Helper returns `Result`; `Err` → spawn failure (G6) |
| R5 | Existing tests break from the signature change | Local projects take the unchanged local branch; only `Option` plumbing (G7) |
| R6 | `Interactive` mismatch allocates a TTY | Pass `Interactive::No` (G10) |
| R7 | `run_on = "local"` on a remote project re-fails via remote `cwd` | Resolver drops `cwd` to `None` on forced-local-over-remote (G5b) |
| R8 | Users relying on the (broken) local-spawn-on-remote behaviour regress when `auto` becomes default | `run_on = "local"` is the documented escape hatch; default `auto` is the fix |

## Effort estimate

~5–7 hours of focused work: a small, well-bounded refactor that mirrors an
existing, proven pattern (`exec_in_shell`), plus a self-contained config enum
for the override. The risk is low because the transport design copies a working
upstream branch rather than inventing one, the test seam (mock transport +
scripted runner) already exists, and the override is additive (`#[serde(default)]`
so existing configs are unaffected).