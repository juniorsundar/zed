use crate::config::{Outcome, RunOn, substitute, substitute_args, substitute_json};
use crate::remote::resolve_command;
use async_trait::async_trait;
use collections::HashMap;
use editor::Editor;
use futures::{AsyncRead, AsyncReadExt as _, future};
use gpui::{App, AppContext as _, TaskExt as _, WeakEntity, Window};
use project::Project;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use util::{ResultExt as _, paths::PathWithPosition};
use workspace::{OpenOptions, Workspace};

const MAX_DIAGNOSTIC_LINE_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct CommandExit {
    pub code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        environment: &HashMap<String, String>,
    ) -> std::io::Result<CommandExit>;
}

struct DefaultCommandRunner;
struct GlobalCommandRunner(Arc<dyn CommandRunner>);

impl gpui::Global for GlobalCommandRunner {}

#[cfg(test)]
pub fn set_command_runner(runner: Arc<dyn CommandRunner>, cx: &mut App) {
    cx.set_global(GlobalCommandRunner(runner));
}

fn command_runner(cx: &App) -> Arc<dyn CommandRunner> {
    cx.try_global::<GlobalCommandRunner>()
        .map(|runner| runner.0.clone())
        .unwrap_or_else(|| Arc::new(DefaultCommandRunner))
}

struct RunningCommand {
    child: util::process::Child,
    exited: bool,
}

impl Drop for RunningCommand {
    fn drop(&mut self) {
        if !self.exited {
            self.child.kill().log_err();
        }
    }
}

#[async_trait]
impl CommandRunner for DefaultCommandRunner {
    async fn run(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        environment: &HashMap<String, String>,
    ) -> std::io::Result<CommandExit> {
        let mut command = util::command::new_std_command(command);
        command.args(args).env_clear().envs(environment);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = util::process::Child::spawn(
            command,
            std::process::Stdio::null(),
            std::process::Stdio::piped(),
            std::process::Stdio::piped(),
        )
        .map_err(std::io::Error::other)?;
        let mut running = RunningCommand {
            child,
            exited: false,
        };
        let stdout = running.child.stdout.take().ok_or_else(|| {
            std::io::Error::other("the command was spawned without a stdout pipe")
        })?;
        let stderr = running.child.stderr.take().ok_or_else(|| {
            std::io::Error::other("the command was spawned without a stderr pipe")
        })?;

        let (stdout, stderr) =
            future::try_join(first_non_empty_line(stdout), first_non_empty_line(stderr)).await?;
        let status = running.child.status().await?;
        running.exited = true;

        Ok(CommandExit {
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

async fn first_non_empty_line(
    mut output: impl AsyncRead + Unpin,
) -> std::io::Result<Option<String>> {
    let mut retained = None;
    let mut line = Vec::new();
    let mut buffer = [0; 8 * 1024];

    loop {
        let read = output.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        if retained.is_some() {
            continue;
        }

        for byte in &buffer[..read] {
            if *byte == b'\n' {
                let candidate = String::from_utf8_lossy(&line).trim().to_owned();
                line.clear();
                if !candidate.is_empty() {
                    retained = Some(candidate);
                    break;
                }
            } else if line.len() < MAX_DIAGNOSTIC_LINE_BYTES {
                line.push(*byte);
            }
        }
    }

    if retained.is_none() {
        let candidate = String::from_utf8_lossy(&line).trim().to_owned();
        if !candidate.is_empty() {
            retained = Some(candidate);
        }
    }
    Ok(retained)
}

pub fn resolve_entry_path(entry: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(entry);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// The Preview resolves the selected Entry the same way, so what it shows is
/// what Confirm will open.
pub fn resolve_outcome_path(
    outcome: &Outcome,
    entry: &str,
    cwd: &Path,
    delimiter: Option<&str>,
) -> Option<Result<PathWithPosition, String>> {
    let template = outcome.path_template()?;
    let resolved = match substitute(template, entry, delimiter) {
        Ok(resolved) => resolved,
        Err(error) => return Some(Err(error.to_string())),
    };

    let mut position = if outcome.path_carries_position() {
        PathWithPosition::parse_str(&resolved)
    } else {
        PathWithPosition::from_path(PathBuf::from(resolved))
    };
    position.path = resolve_entry_path(&position.path.to_string_lossy(), cwd);
    Some(Ok(position))
}

pub fn apply_outcome(
    outcome: &Outcome,
    entry: &str,
    cwd: &Path,
    delimiter: Option<&str>,
    run_on: RunOn,
    workspace: &WeakEntity<Workspace>,
    project: &gpui::Entity<Project>,
    window: &mut Window,
    cx: &mut App,
) {
    match outcome {
        Outcome::OpenPath { .. } | Outcome::OpenPathAtPosition { .. } => {
            match resolve_outcome_path(outcome, entry, cwd, delimiter) {
                Some(Ok(target)) => open_path(target, workspace, window, cx),
                Some(Err(error)) => show_error(workspace, error, cx),
                None => {}
            }
        }
        Outcome::DispatchAction { action, args } => {
            let args = match args
                .as_ref()
                .map(|args| substitute_json(args, entry, delimiter))
                .transpose()
            {
                Ok(args) => args,
                Err(error) => return show_error(workspace, error.to_string(), cx),
            };
            match cx.build_action(action, args) {
                Ok(action) => window.dispatch_action(action, cx),
                Err(error) => {
                    show_error(workspace, format!("`{action}` could not run: {error}"), cx)
                }
            }
        }
        Outcome::RunCommand { command, args } => {
            let args = match substitute_args(args, entry, delimiter) {
                Ok(args) => args,
                Err(error) => return show_error(workspace, error.to_string(), cx),
            };
            let environment = project.update(cx, |project, cx| {
                let worktree = project.visible_worktrees(cx).next()?;
                Some(project.environment().update(cx, |environment, cx| {
                    environment.worktree_environment(worktree, cx)
                }))
            });
            let runner = command_runner(cx);
            let command = command.clone();
            let remote_transport = project.read(cx).remote_client();
            let cwd = cwd.to_path_buf();
            let workspace = workspace.clone();
            let command_workspace = workspace.clone();

            workspace
                .update(cx, |_, cx| {
                    cx.spawn(async move |_, cx| {
                        let environment = match environment {
                            Some(environment) => environment.await,
                            None => None,
                        };
                        let Some(environment) = environment else {
                            command_workspace
                                .update(cx, |workspace, cx| {
                                    workspace.show_error(
                                        format!(
                                            "Could not run `{command}`: the project environment was unavailable"
                                        ),
                                        cx,
                                    )
                                })
                                .log_err();
                            return;
                        };
                        let resolved = match resolve_command(
                            remote_transport.as_ref(),
                            run_on,
                            command.clone(),
                            args.clone(),
                            &cwd,
                            environment,
                            cx,
                        ) {
                            Ok(resolved) => resolved,
                            Err(error) => {
                                command_workspace
                                    .update(cx, |workspace, cx| {
                                        workspace.show_error(
                                            format!("Could not run `{command}`: {error}"),
                                            cx,
                                        )
                                    })
                                    .log_err();
                                return;
                            }
                        };
                        let result = cx
                            .background_spawn(async move {
                                runner
                                    .run(
                                        &resolved.command,
                                        &resolved.args,
                                        resolved.cwd.as_deref(),
                                        &resolved.env,
                                    )
                                    .await
                            })
                            .await;

                        let error = match result {
                            Ok(CommandExit { code: Some(0), .. }) => return,
                            Ok(exit) => command_failure(&command, exit),
                            Err(error) => format!("Could not run `{command}`: {error}"),
                        };
                        command_workspace
                            .update(cx, |workspace, cx| workspace.show_error(error, cx))
                            .log_err();
                    })
                    .detach();
                })
                .log_err();
        }
    }
}

fn command_failure(command: &str, exit: CommandExit) -> String {
    let status = match exit.code {
        Some(code) => format!("exited with status {code}"),
        None => "was terminated by a signal".to_owned(),
    };
    let detail = exit
        .stderr
        .filter(|detail| !detail.trim().is_empty())
        .or_else(|| exit.stdout.filter(|detail| !detail.trim().is_empty()));
    match detail {
        Some(detail) => format!("`{command}` {status}: {detail}"),
        None => format!("`{command}` {status}"),
    }
}

fn open_path(
    target: PathWithPosition,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| {
            let fs = workspace.project().read(cx).fs().clone();
            cx.spawn_in(window, async move |workspace, cx| {
                let exists = fs.metadata(&target.path).await.ok().flatten().is_some();
                let open = workspace.update_in(cx, |workspace, window, cx| {
                    if !exists {
                        workspace
                            .show_error(format!("{} no longer exists", target.path.display()), cx);
                        return None;
                    }
                    Some(workspace.open_abs_path(
                        target.path.clone(),
                        OpenOptions::default(),
                        window,
                        cx,
                    ))
                })?;

                let Some(item) = open else {
                    return anyhow::Ok(());
                };
                let item = item.await?;
                let Some(row) = target.row else {
                    return anyhow::Ok(());
                };
                let Some(editor) = item.downcast::<Editor>() else {
                    return anyhow::Ok(());
                };

                editor
                    .update_in(cx, |editor, window, cx| {
                        let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
                            return;
                        };
                        let snapshot = buffer.read(cx).snapshot();
                        // Rows and columns are 1-based in tool output;
                        // buffer points are 0-based.
                        let point = snapshot.point_from_external_input(
                            row.saturating_sub(1),
                            target.column.unwrap_or(1).saturating_sub(1),
                        );
                        editor.go_to_singleton_buffer_range(point..point, window, cx);
                    })
                    .log_err();
                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        })
        .log_err();
}

fn show_error(workspace: &WeakEntity<Workspace>, message: String, cx: &mut App) {
    workspace
        .update(cx, |workspace, cx| workspace.show_error(message, cx))
        .log_err();
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Mutex;

    pub struct ScriptedCommandRunner {
        result: std::io::Result<CommandExit>,
        invocations: Mutex<Vec<(String, Vec<String>)>>,
        working_directories: Mutex<Vec<Option<PathBuf>>>,
    }

    impl ScriptedCommandRunner {
        pub fn succeeding() -> Self {
            Self::returning(CommandExit {
                code: Some(0),
                stdout: None,
                stderr: None,
            })
        }

        pub fn returning(exit: CommandExit) -> Self {
            Self {
                result: Ok(exit),
                invocations: Mutex::new(Vec::new()),
                working_directories: Mutex::new(Vec::new()),
            }
        }

        pub fn failing_to_spawn(reason: &str) -> Self {
            Self {
                result: Err(std::io::Error::other(reason.to_owned())),
                invocations: Mutex::new(Vec::new()),
                working_directories: Mutex::new(Vec::new()),
            }
        }

        pub fn invocations(&self) -> Vec<(String, Vec<String>)> {
            self.invocations.lock().expect("unpoisoned").clone()
        }

        pub fn working_directories(&self) -> Vec<Option<PathBuf>> {
            self.working_directories.lock().expect("unpoisoned").clone()
        }
    }

    #[async_trait]
    impl CommandRunner for ScriptedCommandRunner {
        async fn run(
            &self,
            command: &str,
            args: &[String],
            cwd: Option<&Path>,
            _environment: &HashMap<String, String>,
        ) -> std::io::Result<CommandExit> {
            self.invocations
                .lock()
                .expect("unpoisoned")
                .push((command.to_owned(), args.to_vec()));
            self.working_directories
                .lock()
                .expect("unpoisoned")
                .push(cwd.map(|cwd| cwd.to_path_buf()));
            match &self.result {
                Ok(exit) => Ok(exit.clone()),
                Err(error) => Err(std::io::Error::new(error.kind(), error.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_entry_is_used_as_is() {
        assert_eq!(
            resolve_entry_path("/etc/hosts", Path::new("/project")),
            PathBuf::from("/etc/hosts")
        );
    }

    #[test]
    fn a_relative_entry_resolves_against_the_working_directory() {
        assert_eq!(
            resolve_entry_path("src/main.rs", Path::new("/project")),
            PathBuf::from("/project/src/main.rs")
        );
    }

    fn resolve(outcome: &Outcome, entry: &str, delimiter: Option<&str>) -> PathWithPosition {
        resolve_outcome_path(outcome, entry, Path::new("/project"), delimiter)
            .expect("this outcome opens a path")
            .expect("the template applied")
    }

    #[test]
    fn open_path_defaults_to_the_whole_entry() {
        let resolved = resolve(&Outcome::OpenPath { path: None }, "src/main.rs", None);
        assert_eq!(resolved.path, PathBuf::from("/project/src/main.rs"));
        assert_eq!(resolved.row, None);
    }

    /// `git status --porcelain` puts the path in the second Field.
    #[test]
    fn open_path_can_name_a_field() {
        let resolved = resolve(
            &Outcome::OpenPath {
                path: Some("{2}".into()),
            },
            " M src/main.rs",
            None,
        );
        assert_eq!(resolved.path, PathBuf::from("/project/src/main.rs"));
    }

    /// Parsed from the right, so a path may contain the delimiter.
    #[test]
    fn open_path_at_position_reads_a_trailing_row_and_column() {
        let resolved = resolve(
            &Outcome::OpenPathAtPosition { path: None },
            "src/main.rs:12:3",
            Some(":"),
        );
        assert_eq!(resolved.path, PathBuf::from("/project/src/main.rs"));
        assert_eq!(resolved.row, Some(12));
        assert_eq!(resolved.column, Some(3));
    }

    /// A ripgrep line: only the first three Fields are the target; the text
    /// may itself contain the delimiter.
    #[test]
    fn a_ripgrep_line_resolves_to_its_file_and_position() {
        let resolved = resolve(
            &Outcome::OpenPathAtPosition {
                path: Some("{1}:{2}:{3}".into()),
            },
            "src/main.rs:12:3:let ratio = a:b;",
            Some(":"),
        );
        assert_eq!(resolved.path, PathBuf::from("/project/src/main.rs"));
        assert_eq!(resolved.row, Some(12));
        assert_eq!(resolved.column, Some(3));
    }

    #[test]
    fn open_path_does_not_read_a_row_from_a_file_named_like_one() {
        let resolved = resolve(&Outcome::OpenPath { path: None }, "notes:2024", None);
        assert_eq!(resolved.path, PathBuf::from("/project/notes:2024"));
        assert_eq!(resolved.row, None);
    }

    #[test]
    fn naming_a_field_an_entry_does_not_have_is_reported() {
        let error = resolve_outcome_path(
            &Outcome::OpenPath {
                path: Some("{3}".into()),
            },
            "one two",
            Path::new("/project"),
            None,
        )
        .expect("this outcome opens a path")
        .expect_err("the entry has no third field");

        assert!(error.contains("field 3"), "got {error:?}");
    }

    #[test]
    fn command_diagnostics_skip_blank_lines_and_bound_the_first_detail() {
        let long_detail = "x".repeat(MAX_DIAGNOSTIC_LINE_BYTES + 100);
        let output = format!("\n  \r\n{long_detail}\nignored\n");
        let retained = futures::executor::block_on(first_non_empty_line(futures::io::Cursor::new(
            output.into_bytes(),
        )))
        .expect("output drained")
        .expect("diagnostic retained");

        assert_eq!(retained, "x".repeat(MAX_DIAGNOSTIC_LINE_BYTES));
    }

    #[test]
    fn a_command_failure_prefers_stderr_and_reports_the_exit_status() {
        let message = command_failure(
            "git",
            CommandExit {
                code: Some(128),
                stdout: Some("stdout detail".into()),
                stderr: Some("fatal: invalid reference".into()),
            },
        );

        assert_eq!(
            message,
            "`git` exited with status 128: fatal: invalid reference"
        );
    }

    #[test]
    fn a_command_failure_uses_stdout_when_stderr_is_empty() {
        let message = command_failure(
            "git",
            CommandExit {
                code: Some(1),
                stdout: Some("stdout detail".into()),
                stderr: Some(String::new()),
            },
        );

        assert_eq!(message, "`git` exited with status 1: stdout detail");
    }

    #[test]
    fn dispatch_action_has_no_path_to_resolve() {
        assert!(
            resolve_outcome_path(
                &Outcome::DispatchAction {
                    action: "zed::OpenBrowser".into(),
                    args: None,
                },
                "https://zed.dev",
                Path::new("/project"),
                None,
            )
            .is_none()
        );
    }
}
