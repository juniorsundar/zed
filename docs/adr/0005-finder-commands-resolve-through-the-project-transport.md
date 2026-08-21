# Finder commands resolve through the project transport before spawn

## Status

Proposed

## Context

The Finder spawns two kinds of child process: a **Source** command that
produces the entry list (`rg`, `git branch`, …) and a **`RunCommand`** outcome
that runs on confirm. Both runners (`DefaultSourceRunner`,
`DefaultCommandRunner`) call `util::command::new_command`/`new_std_command`
directly — a **local** `smol`/`std` process — and set `.current_dir(cwd)` to the
first worktree's `abs_path()`.

When the open project is remote (SSH / WSL / Docker), that `abs_path()` is a
**remote path** that does not exist on the local host, and the binary (`rg`)
is searched for on the **local** `PATH`. The spawn fails with "No such file or
directory" instead of detecting the binary on the remote.

The Finder crate contains zero references to `remote`/`is_remote`. It does
fetch the environment correctly (`worktree_environment` →
`remote_directory_environment`, an RPC to the remote server), but then hands
that remote env to a *locally-spawned* process whose `cwd` and binary
resolution are local — so the remote `PATH` it carries is never used.

Every other command-spawning subsystem in Zed (`Project::exec_in_shell`,
context servers, terminals) already solves this by branching on
`project.remote_client()` and calling
`RemoteClient::build_command(..) -> CommandTemplate`, whose `program` is the
transport launcher (`ssh`, the WSL launcher, `docker exec`) and whose args
embed `cd <dir> ; exec env K=V … <program> <args>`. The Finder was written
assuming a local project and skipped that branch.

## Decision

Resolve the command through the project transport **at the call site**, then
hand the already-resolved `(program, args, env, cwd)` to the existing,
transport-agnostic runner. Add a per-finder `run_on` policy so individual
finders can override where they execute. Concretely:

- Add `RunOn { Auto, Local }` (default `Auto`) as a top-level `run_on = "…"` key
  in the finder TOML. `Auto` follows the project; `Local` forces execution on
  this machine. Whole-finder: it governs both the Source spawn and the
  `RunCommand` outcome; `OpenPath`/`DispatchAction` outcomes ignore it (they
  never spawn a process).
- Add `resolve_command_for_project(project, run_on, command, args, cwd, env, cx)`.
- **Gate on `project.remote_client().is_some()`**, not `is_remote()`, so
  WSL-with-host-interop (where `is_remote()` is false but `remote_client` is
  `Some`) is covered alongside SSH and Docker. This mirrors `exec_in_shell`.
- **`Auto` + remote client:** call
  `remote_client.build_command(Some(command), &args, &env, Some(cwd), None, Interactive::No)`,
  return `(template.program, template.args, template.env, None)` — the `None`
  cwd means "do not set a local `current_dir`; the `cd` is inside the ssh args".
  The **spawn env** becomes the template's env (the SSH *connection* env); the
  project env was baked into the ssh arg string by `build_command`.
- **`Auto`/`Local` + no remote client:** local spawn, return
  `(command, args, env, Some(cwd))` — today's behaviour, unchanged.
- **`Local` + remote client:** forced-local-on-remote. Return
  `(command, args, env, None)` — spawn locally but **drop `cwd`**, because the
  worktree `abs_path()` is a remote path that a local `current_dir()` cannot
  use (it would re-fail the original bug). The user explicitly asked for local
  execution, so they own making the command work without the remote project as
  its working directory.
- **Error:** if `build_command` returns `Err` (e.g. dropped connection), surface
  it as a spawn failure (`SourceFailure::CouldNotSpawn` / `show_error`), **not**
  a silent local fallback — falling back to local would re-introduce the
  original bug.

The three call sites are `FinderDelegate::spawn_source`,
`FinderDelegate::update_query_matches`, and `apply_outcome`'s `RunCommand`
branch. The `SourceRunner`/`CommandRunner` traits stay transport-agnostic;
only `cwd` widens to `Option` so the runner can express "the `cd` is baked into
the args, or omitted on forced-local-over-remote". `Interactive::No` is used
because Sources and `RunCommand` communicate via piped stdio, not a TTY.

## Consequences

- The runner traits keep their shape (a `command/args/cwd/env` seam) and stay
  testable via the existing `ScriptedRunner`/`ScriptedCommandRunner` injection.
  The remote transform is observable because the mock transport's
  `build_command` returns `program = "mock"`, so a remote-finder test can
  assert the runner was invoked with `"mock"` rather than the raw `rg`.
- Existing local-project tests are unaffected: `Project::test` has no
  `remote_client`, so the local branch is taken and behaviour is identical;
  only the `Option<&Path>` plumbing changes in test call sites.
- Preview paths (`OpenPath`/`OpenPathAtPosition`) are untouched — they already
  resolve through the remote-aware `Project`/`Fs`. `DispatchAction` is local by
  design and is also untouched. Scope is exactly the two command runners.
- `run_on` is additive: `#[serde(default)]` means existing finder configs parse
  unchanged and get `Auto` (the fix). Users who relied on the (broken)
  local-spawn-on-remote behaviour can set `run_on = "local"` as the documented
  escape hatch.
- The fork's upstream diff grows by one new module, the `RunOn` enum, and the
  `Option` cwd tweak; no change to `RemoteClient` or the transports (we only
  *consume* `build_command`), so rebase cost stays low.
- Getting the gate wrong (`is_remote()` instead of `remote_client().is_some()`)
  would silently miss WSL; getting the env wrong (using the project env for the
  local `ssh` spawn instead of `template.env`) would leak local/remote PATH
  confusion back in; dropping `cwd` on forced-local-over-remote is essential —
  passing the remote path to a local `current_dir` re-fails the original way.
  All three are called out in `REMOTE_SCOPE.md` (R1, R2, R7).