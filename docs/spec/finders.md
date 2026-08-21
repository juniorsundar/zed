# Finders

Config-driven fuzzy pickers, defined in TOML and usable without rebuilding Zed.

Vocabulary in this document follows `CONTEXT.md`: a **Finder** is a named
configuration, a **Source** produces its **Entries**, and an **Outcome** is what
happens when an Entry is chosen. See also
`docs/adr/0001-finders-dispatch-via-a-parameterized-action.md`. Remote
execution is recorded in
`docs/adr/0005-finder-commands-resolve-through-the-project-transport.md`.

## Problem Statement

Zed ships around fifty fuzzy pickers — files, commands, branches, symbols,
themes — and every one of them is a Rust type compiled into the binary. A user
who wants a picker over something Zed does not already model (running
containers, Kubernetes pods, a script's output, a bespoke project index) has no
route to one. The extension API does not expose pickers at all: it covers
language servers, themes, and slash commands only.

The result is that a user with a two-line shell command that produces exactly
the list they want to fuzzy-search has no way to search it inside the editor,
and must leave for a terminal.

When the open project is remote (SSH, WSL, or Docker), a Finder that worked
locally fails instead of detecting the binary on the remote host. The Source
command and any `run_command` Outcome are spawned as **local** processes with
the worktree's `abs_path()` as the working directory — a remote path that does
not exist on the local machine — and the binary (`rg`) is searched for on the
**local** `PATH`. The Finder already fetches the remote host's environment
correctly, but hands it to a locally-spawned process, so that environment is
never used to find the binary. Every other command-spawning subsystem in Zed
already solves this by resolving the command through the project's remote
transport before spawning; the Finder was written assuming a local project and
skipped that step.

## Solution

A user defines a Finder in a TOML file: a name, a command whose stdout supplies
the Entries, and what should happen when one is chosen. Saving the file makes
the Finder available immediately — no rebuild, no restart.

Finders are opened either from a list of all configured Finders, or from a
keybinding that names one directly. Once open, a Finder looks and behaves like
any other Zed picker: type to fuzzy-filter, arrow to select, enter to act.

On a remote project, a Finder's Source command and `run_command` Outcome are
resolved through the project's remote transport (SSH, WSL, or Docker) before
they are spawned, so the binary is detected and run on the remote host with the
remote working directory and environment. A per-Finder `run_on` policy lets an
author force a particular Finder to run on the local machine even when a remote
project is open, for Finders that must query the local host.

## User Stories

### Authoring Finders

1. As a Finder author, I want to define a Finder in a single TOML file, so that I can add a fuzzy list without writing Rust.
2. As a Finder author, I want Finders keyed by name in the config, so that defining the same name twice is rejected by the config format itself rather than silently resolving to one of them.
3. As a Finder author, I want to give a Finder a human-readable label, so that the list of Finders reads as names rather than slugs.
4. As a Finder author, I want to set a custom placeholder for the query field, so that the picker explains what it is searching.
5. As a Finder author, I want the label and placeholder to be optional with sensible defaults, so that a minimal Finder is genuinely minimal.
6. As a Finder author, I want my edits to take effect when I save the file, so that I can iterate on a Finder in seconds rather than restarting the editor.
7. As a Finder author, I want a syntax error in one Finder to leave my other Finders working, so that one typo does not disable the whole feature.
8. As a Finder author, I want to see the actual parse error, so that I can fix it without bisecting the file.
9. As a Finder author, I want unrecognised config keys to be rejected rather than ignored, so that a misspelled key fails loudly instead of silently taking a default.
10. As a Finder author, I want the Source declaration to carry an explicit type tag, so that adding other kinds of Source later does not invalidate the configs I have already written.

### Reaching a Finder

11. As a Zed user, I want to bind a key to a specific Finder, so that I can open the ones I use constantly without going through a list.
12. As a Zed user, I want to add a keybinding for a brand-new Finder without rebuilding Zed, so that the feature is genuinely extensible after installation.
13. As a Zed user, I want a picker listing every configured Finder, so that I can use a new Finder immediately without first inventing a keybinding for it.
14. As a Zed user, I want the Finder list to show Finders that failed to load, so that a broken config is visible in the place I would naturally look.
15. As a Zed user, I want an error when I open a Finder name that does not exist, so that a typo in my keymap is not an unresponsive key.
16. As a Zed user, I want a distinct error when the Finder exists in the config but failed to parse, so that I know to fix the config rather than the keybinding.

### Running a Finder

17. As a Zed user, I want the picker to appear the moment I press the key, so that a slow Source never freezes the editor.
18. As a Zed user, I want to see that the Source is still running, so that an empty list is not mistaken for "no results".
19. As a Zed user, I want Entries fuzzy-matched as I type, with the same matching behaviour as the rest of Zed's pickers.
20. As a Zed user, I want the Source re-run each time I open the Finder, so that I never act on a stale list.
21. As a Zed user, I want the Source command run in the project's resolved environment, so that tools provided by a project shell or `.envrc` are found rather than reported missing.
22. As a Zed user, I want the Source command run at the project root, so that relative paths in its output mean what I expect.
23. As a Zed user, I want an error when I open a Finder with no project open, rather than results computed from an arbitrary directory.

### When a Source misbehaves

24. As a Zed user, I want a missing binary reported inside the picker, so that I can diagnose it without opening a log file.
25. As a Zed user, I want a non-zero exit reported inside the picker, so that a failing command is distinguishable from one that legitimately produced nothing.
26. As a Zed user, I want a hung Source to time out, so that a misconfigured Finder does not leave a permanently empty picker open.
27. As a Zed user, I want oversized Source output to fail with a message naming the cause, so that I learn the Finder needs a different approach rather than watching memory climb.

### Acting on an Entry

28. As a Zed user, I want to open the selected Entry as a file, so that path-producing Finders behave like the file finder.
29. As a Zed user, I want absolute paths in Entries honoured as-is, so that Sources that emit absolute paths work without configuration.
30. As a Zed user, I want to open a file that lies outside any worktree, so that Finders are not restricted to the current project's tree.
31. As a Zed user, I want an error when the selected path no longer exists, rather than silently being given a new empty buffer.
32. As a Zed user, I want to dispatch any Zed action with the selected Entry substituted into its arguments, so that a Finder can drive anything Zed can already do.
33. As a Zed user, I want an unrecognised or malformed action reported as an error, so that a mistake in my Outcome config is visible.
34. As a Zed user, I want the same `{}` substitution convention everywhere it appears, so that there is one rule to remember.

### Maintaining the fork

35. As a fork maintainer, I want Finder code isolated in its own crate, so that upstream changes rarely touch files I have modified.
36. As a fork maintainer, I want the diff against upstream limited to three lines, so that rebasing onto upstream `main` stays cheap indefinitely.

### Running a Finder on a remote project

37. As a Zed user editing a project over SSH, I want a Finder's Source command to run on the remote host, so that `rg` and project tools present there are found rather than reported missing on the local machine.
38. As a Zed user editing a project over SSH, I want a `run_command` Outcome to run on the remote host, so that acting on an Entry executes in the project's context rather than locally.
39. As a Zed user, I want the remote working directory embedded in the remote command, so that relative paths in Source output resolve on the remote host rather than against a local directory that does not exist.
40. As a Zed user editing over WSL, I want my Finders to run inside the WSL distribution, so that they behave the same as over SSH rather than silently falling back to the Windows host because the project's client state is local.
41. As a Zed user editing over Docker, I want my Finders to run inside the container, so that container-local binaries are found.
42. As a Zed user, I want the remote host's environment the Finder already fetches to actually drive the spawn, so that remote-only binaries on a non-default `PATH` are discovered.
43. As a Zed user, I want a dropped remote connection reported as a Finder failure, rather than the command silently running locally against a path that does not exist.

### Overriding where a Finder runs

44. As a Finder author, I want to declare that a particular Finder always runs on this machine, so that a Finder querying the local host keeps working even when I have a remote project open.
45. As a Finder author, I want Finders that do not opt in to keep running as before when I open a remote project, so that the remote fix does not require me to edit every Finder I have written.
46. As a Finder author, I want an unknown `run_on` value rejected, consistent with how unknown config keys are already treated, so that a typo fails loudly.
47. As a Finder author, I want `run_on` to apply to both the Source and a `run_command` Outcome, so that one declaration controls the whole Finder's execution location.
48. As a Zed user, I want `open_path` and `dispatch_action` Outcomes unaffected by `run_on`, so that opening files and dispatching actions keep working the same way regardless of the policy.
49. As a Finder author forcing local execution on a remote project, I want the command run without the remote working directory, so that it is not handed a path that does not exist on this machine.

## Implementation Decisions

### Configuration

Finders are declared in a single TOML file in Zed's config directory, as tables
keyed by Finder name. Keying by name rather than using an array of tables means
TOML itself rejects duplicate definitions.

```toml
[finder.git-branches]
label = "Git Branches"
placeholder = "Switch branch…"
source = { type = "command", command = "git", args = ["for-each-ref", "--format=%(refname:short)", "refs/heads"] }
outcome = { type = "dispatch_action", action = "task::Spawn", args = { task_name = "checkout-{}" } }

[finder.project-files]
source = { type = "command", command = "git", args = ["ls-files"] }
outcome = { type = "open_path" }
```

`source` and `outcome` are internally tagged enums. Both carry a `type` tag in
v1 despite `source` having only one variant, so that adding query-driven
Sources later is not a breaking config change. Unknown fields are rejected;
TOML has no editor schema support in Zed, so a misspelled key must fail rather
than default.

`label` defaults to the table key. `placeholder` defaults to a phrase derived
from the label.

The file is watched and reloaded on save, reusing the existing settings-file
watching helper. A Finder whose TOML fails to parse is retained in the registry
as a keyed error rather than being dropped, so that opening it can report the
parse failure instead of claiming the Finder does not exist.

### Run policy

Each Finder carries an optional top-level `run_on` policy controlling where its
command-spawning work executes. It is a two-variant enum, defaulting to `auto`:

```toml
[finder.local-only]
run_on = "local"
source = { type = "command", command = "rg", args = ["--files"] }
outcome = { type = "open_path" }
```

- `auto` (default): follow the project. When a remote client is present, the
  Source and `run_command` Outcome resolve through the remote transport; when
  none is present, they spawn locally as before. This is the fix for the
  remote-project failure, and because it is the default, existing Finder
  configurations gain the fix without edits.
- `local`: always spawn on this machine, even when a remote project is open.
  An escape hatch for Finders that must query the local host (for example, a
  Finder over locally running containers) while a remote project is open.

`run_on` is whole-Finder: it governs both the Source spawn and a
`run_command` Outcome. `open_path`, `open_path_at_position`, and
`dispatch_action` Outcomes ignore it, because they never spawn a process —
path opening resolves through the remote-aware project filesystem, and action
dispatch is local by design. The field is additive with a serde default, so
existing configurations parse unchanged and receive `auto`.

An unknown value (`run_on = "mars"`) is rejected: serde denies unknown enum
variants, so the offending Finder is retained as a keyed parse error like any
other malformed Finder, consistent with the unknown-key rejection above.

### Dispatch

Finders are reached through a single statically registered action carrying the
Finder name as a parameter, plus a parameterless action that opens the Finder
list. GPUI collects actions at link time, so no new action type can be
registered at runtime; the parameterized action is the mechanism that makes
post-build extensibility possible. This is recorded as ADR 0001 and follows
the same pattern upstream uses for spawning named tasks.

Because the keymap cannot validate Finder names, unknown and failed-to-parse
Finders must surface as user-visible errors at dispatch time.

### Execution

The picker opens immediately and populates asynchronously. This is required so
that a slow Source cannot freeze the window, and it also shapes the delegate
around "Entries arrive later, then notify", which is the same shape a streaming
Source will need.

The Source runs on every open with no caching between opens, so that a Finder
never presents stale data.

The working directory is the first worktree root, not configurable in v1.
Opening a Finder with no worktree is refused with an error rather than falling
back to a guessed directory.

The environment is the project's resolved worktree environment, not Zed's
process environment. Zed captures login-shell environment at startup from the
home directory, which does not include per-project environment from tools like
direnv, mise, or a Nix development shell. Without this, Sources referencing
project-provided binaries fail with "command not found" despite working in the
user's terminal.

Execution is bounded by a timeout and a maximum output size. Exceeding either,
along with spawn failure and non-zero exit, produces zero Entries and surfaces
the reason through the picker's existing empty-state text, requiring no new UI.
The oversize message should name output size as the cause, since that condition
is precisely the signal that the Finder requires streaming.

### Remote execution

The Source and `run_command` commands are resolved through the project's
transport **before** they are spawned, rather than spawned directly. The
resolution happens at the call site — where the `Project` entity is available —
not inside the process-spawning runner, so the runner stays transport-agnostic
and its existing test injection is preserved.

Resolution branches on the project's remote client, **not** on `is_remote()`.
`is_remote()` is false for WSL-with-host-interop even though a remote client is
present, so gating on `is_remote()` would silently miss WSL; gating on the
remote client's presence covers SSH, WSL, and Docker in a single branch, and
mirrors how `Project::exec_in_shell` and the terminal system already work.

Under `auto`, when a remote client is present the command is passed to the
remote client's command builder, which returns a transport-resolved command
template: for SSH the program becomes `ssh` and the args embed
`cd <dir> ; exec env K=V … <program> <args>`. The resolved program, args, and
the template's own environment are what get spawned locally — the **spawn
environment** is the template's environment (the SSH connection environment),
not the project environment, because the project environment was already baked
into the ssh command string by the builder. The spawn sets **no** local working
directory, because the `cd` is inside the ssh args; handing the remote
`abs_path()` to a local `current_dir` is the original failure.

Under `auto` with no remote client, and under `local` with no remote client,
the command spawns locally with the worktree root as the working directory —
today's behaviour, unchanged.

Under `local` with a remote client present, the command spawns locally but the
working directory is **dropped**. The worktree `abs_path()` is a remote path that
a local `current_dir` cannot use; passing it would re-fail the original way.
The author explicitly asked for local execution, so they own making the command
work without the remote project as its working directory. This keeps `local` a
working escape hatch rather than a guaranteed error.

If the remote command builder returns an error — for example, because the
connection dropped — it surfaces as a spawn failure (the picker's
could-not-spawn state, or the workspace error for a `run_command` Outcome),
**not** a silent local fallback. Falling back to local would re-introduce the
original bug: spawning the raw binary against a remote path.

`Interactive::No` is requested when building the remote command, because
Sources and `run_command` communicate via piped stdio rather than a terminal;
the transport uses it to avoid allocating a pseudo-TTY.

### Entries and matching

An Entry is one trimmed line of stdout. The displayed text and the value handed
to the Outcome are the same string; Sources are expected to emit clean output,
which is nearly always achievable with a formatting flag. Matching uses Zed's
existing fuzzy string matcher, client-side over the accumulated Entries.

### Outcomes

The initial release had two Outcome variants; later additions extend the same
tagged enum.

`open_path` treats the Entry as absolute if it is absolute, otherwise joins it
to the working directory, and opens it through the workspace's absolute-path
open path so that files outside any worktree are supported. A path that does
not exist is an error, not an invitation to create a file. Path opening is
remote-aware through the project filesystem and is unaffected by `run_on`.

`dispatch_action` substitutes the selected Entry for `{}` in every string leaf
of the action's argument value, converts those arguments to the JSON
representation the action registry expects, and builds and dispatches the
action by name. The same `{}` convention applies in Source arguments.
`dispatch_action` runs a local GPUI action and is unaffected by `run_on`.

The `run_command` Outcome runs a selected command headlessly:

```toml
outcome = { type = "run_command", command = "git", args = ["checkout", "{1}"] }
```

It executes `command` directly with `args`, without a shell. The executable is
fixed; the existing `{}` and `{n}` substitutions apply only to arguments and
report the same missing-Field errors as other Outcomes. It accepts no shell,
working-directory, environment, or timeout fields.

Confirm dismisses the Finder immediately and starts the command headlessly with
null stdin. The command uses the Source's effective working directory,
including any future configured Source working directory, and the project's
resolved environment. It runs without a timeout in a Finder-owned detached task,
so dismissing the Picker does not cancel it; its process tree is terminated when
the application shuts down.

A `run_command` Outcome resolves through the project transport under the
Finder's `run_on` policy exactly as the Source does, so on a remote project the
command runs on the remote host under the remote working directory and
environment.

Success is quiet. Stdout and stderr are drained so they cannot block the
process, but only a bounded first non-empty line from each is retained. A spawn
failure or substitution error is reported through the workspace. A non-zero
exit reports its status and the retained stderr line, falling back to stdout
when stderr is empty. A remote-transport error is reported as a spawn failure.
Users who need a visible, interactive, or monitored command should use
`dispatch_action` with `task::Spawn` instead.

Command-driven Previews remain a separate feature with their own execution,
cancellation, and output-visibility contract.

### Structure

A new crate holds the config schema, the registry, the Finder picker delegate,
and the Finder list picker. The upstream-facing diff is three lines: a
workspace member entry, a dependency on the new crate from the Zed binary
crate, and one initialisation call placed alongside the other picker
initialisations. Every other file is new and therefore cannot conflict on
rebase.

### The execution seam

Source execution sits behind an injectable trait, mirroring the existing
command-runner trait in the dev container crate. The seam is placed *below* the
timeout and size-cap logic so that those remain production code under test:

```rust
#[async_trait]
pub trait SourceRunner: Send + Sync {
    async fn spawn(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
    ) -> Result<SourceStream, std::io::Error>;
}
```

The default implementation spawns a real process. The test implementation
returns canned output and can await a timer, which lets the deterministic test
executor drive the timeout path without a genuinely slow process. The runner
stays transport-agnostic: it receives an already-resolved command (which, on a
remote project, is `ssh …` rather than the raw binary), so the seam does not
need to know about remotes. The only widening is the working directory becoming
optional, so the runner can express "the `cd` is baked into the args, or
omitted on forced-local-over-remote" instead of always setting `current_dir`.

The remote transport is **not** a new seam. It is resolved at the call site
using the project's existing remote client, and observed in tests through the
codebase's existing remote-test harness — a mock remote client whose command
builder returns a deterministic, recognizable program — rather than through any
new injection point. This keeps the number of seams the Finder introduces at
one.

## Testing Decisions

A good test here exercises observable behaviour: what the user sees in the
picker, what ends up open in the workspace, what the registry reports, and — for
remote execution — which program the runner was asked to spawn. Tests should
not assert on internal match ordering, private field values, or the number of
times the runner was called. Where a test needs to observe a dispatched action,
it should register a recording handler for a test action rather than reaching
into dispatch internals.

Four levels, in descending order of how many tests belong at each.

**Config parsing and substitution.** Pure functions over strings — no GPUI, no
filesystem, no executor. Covers valid configs, unknown-key rejection, missing
required fields, duplicate keys, default derivation for label and placeholder,
and `{}` substitution across nested argument structures including the case
where no placeholder is present. For the run policy it covers `run_on = "local"`
parsing, the `auto` default when the field is omitted, and rejection of an
unknown value. This level should carry the bulk of the tests because it is
where the config contract lives and it costs nothing to run.

**Registry loading and reload.** Uses the existing fake filesystem and the
deterministic executor, following the pattern used for settings and editorconfig
file watching. Covers initial load, reload on write, a Finder that becomes valid
after being broken, a broken Finder leaving its siblings intact, and the
distinction between an absent Finder and one retained as a parse error.

**End-to-end picker behaviour.** Mirrors the file finder's test harness — build a
workspace over a fake filesystem, dispatch the action, then inspect the picker
delegate. Prior art for every part of this exists in the file finder tests:
constructing the workspace and opening the picker, simulating typed input,
advancing the clock past debounces, and asserting on the resulting match list.
Covers the picker opening before Entries arrive, Entries populating, filtering
by typed query, both Outcome variants, and each Source failure mode surfacing
its reason in the empty-state text.

Timeout and oversize tests belong at this level and depend on the injected
runner: the fake awaits a timer that the test advances past the timeout, and
returns oversized canned output for the size-cap case.

**Remote and run-policy behaviour.** Builds a remote project using the
codebase's existing mock remote client and its mock transport, whose command
builder returns a recognizable program rather than the raw binary. The injected
scripted runner records the program it was asked to spawn, so the test asserts
the transform happened. Covers: a Finder on a remote project spawns the
transport program (not the raw binary) with no local working directory; the
same Finder with `run_on = "local"` spawns the raw binary locally with no
working directory, proving the override bypasses the transport and drops the
remote path; and a remote-transport error surfaces as a spawn failure rather
than a local fallback. Existing local-project tests are unaffected because a
test project has no remote client and so takes the unchanged local branch; only
the optional-working-directory plumbing in test call sites changes.

## Out of Scope

Streaming Sources. v1 reads the Source's output to completion before showing
Entries; the size cap exists to make the absence of streaming an explicit,
diagnosable failure rather than a memory problem.

Previews. The picker crate supports them and the config schema leaves room, but
no preview is rendered in v1.

Query-driven Sources — passing the live query into the Source command so that
the process performs the filtering. The `type` tag on `source` exists so this
can be added without breaking existing configs.

Configurable working directory, splitting Entry display text from the value
passed to the Outcome, caching Source output between opens, and splitting the
config into a directory of per-Finder files. All are additive against this
schema.

A `remote` value for `run_on` that forces remote execution. It is omitted
because it would be identical to `auto` whenever a remote client is present, and
impossible (no transport to route through) when one is absent; a forced-local
`local` value is the only meaningful override.

Changing the `RemoteClient` or any transport. The Finder only *consumes* the
existing command-builder; it does not modify remote machinery.

Upstreaming. This is a personal fork; the three-line upstream diff is a
maintenance decision, not a preparation for a pull request.

## Further Notes

This document lives outside the mdbook navigation and is not part of Zed's
published documentation.

The environment decision is the one place the design deliberately exceeds the
minimum. It was made because the fork's author works inside Nix development
shells, where the failure it prevents — a Source that works in the terminal and
reports "command not found" in the editor — would be both immediate and hard
to attribute.

The remote-execution decision deliberately copies a pattern the codebase
already proves (`Project::exec_in_shell`) rather than inventing one, and gates
on the remote client rather than `is_remote()` specifically so WSL is not
missed. The riskiest detail is the forced-local-over-remote working-directory
handling: dropping the working directory is essential, because passing the
remote `abs_path()` to a local `current_dir` is exactly the failure the feature
exists to fix.

Each deferred item above was checked against the config schema during design:
none requires a breaking change to configs written for v1.