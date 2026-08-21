use crate::config::Source;
use async_trait::async_trait;
use collections::HashMap;
use futures::future::{self, Either};
use gpui::{BackgroundExecutor, SharedString};
use std::{fmt, path::Path, process::Output, sync::Arc, time::Duration};

pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024;

/// The seam between a Source declaration and a real process.
///
/// It sits below the timeout and size cap in [`run_source`] so that those stay
/// under test: a test runner can return canned output, or await a timer that
/// the test executor advances past [`SOURCE_TIMEOUT`].
#[async_trait]
pub trait SourceRunner: Send + Sync {
    async fn run(
        &self,
        command: &str,
        args: &[String],
        cwd: &Path,
        env: &HashMap<String, String>,
    ) -> Result<Output, std::io::Error>;
}

pub struct DefaultSourceRunner;

struct GlobalSourceRunner(Arc<dyn SourceRunner>);

impl gpui::Global for GlobalSourceRunner {}

/// Replaces the runner every Finder uses. Tests install a fake here.
pub fn set_source_runner(runner: Arc<dyn SourceRunner>, cx: &mut gpui::App) {
    cx.set_global(GlobalSourceRunner(runner));
}

pub fn source_runner(cx: &gpui::App) -> Arc<dyn SourceRunner> {
    cx.try_global::<GlobalSourceRunner>()
        .map(|global| global.0.clone())
        .unwrap_or_else(|| Arc::new(DefaultSourceRunner))
}

#[async_trait]
impl SourceRunner for DefaultSourceRunner {
    async fn run(
        &self,
        command: &str,
        args: &[String],
        cwd: &Path,
        env: &HashMap<String, String>,
    ) -> Result<Output, std::io::Error> {
        util::command::new_command(command)
            .args(args)
            .current_dir(cwd)
            .envs(env)
            .kill_on_drop(true)
            .output()
            .await
    }
}

/// Why a Source produced no Entries. Rendered into the picker's empty state.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceFailure {
    CouldNotSpawn { command: String, reason: String },
    Failed { command: String, details: String },
    TimedOut { command: String },
    TooMuchOutput { command: String, bytes: usize },
}

impl fmt::Display for SourceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceFailure::CouldNotSpawn { command, reason } => {
                write!(formatter, "Could not run `{command}`: {reason}")
            }
            SourceFailure::Failed { command, details } => {
                write!(formatter, "`{command}` failed: {details}")
            }
            SourceFailure::TimedOut { command } => write!(
                formatter,
                "`{command}` did not finish within {} seconds",
                SOURCE_TIMEOUT.as_secs()
            ),
            SourceFailure::TooMuchOutput { command, bytes } => write!(
                formatter,
                "`{command}` produced too much output ({} MB); this source needs streaming",
                bytes / (1024 * 1024)
            ),
        }
    }
}

/// Runs a Source to completion, bounded by [`SOURCE_TIMEOUT`] and
/// [`MAX_OUTPUT_BYTES`], and splits its stdout into Entries.
pub async fn run_source(
    runner: Arc<dyn SourceRunner>,
    source: Source,
    cwd: Arc<Path>,
    env: HashMap<String, String>,
    executor: BackgroundExecutor,
) -> Result<Vec<SharedString>, SourceFailure> {
    let Source::Command { command, args } = source;

    let run = Box::pin(async {
        runner
            .run(&command, &args, cwd.as_ref(), &env)
            .await
            .map_err(|error| SourceFailure::CouldNotSpawn {
                command: command.clone(),
                reason: error.to_string(),
            })
    });
    let timer = Box::pin(executor.timer(SOURCE_TIMEOUT));

    let output = match future::select(run, timer).await {
        Either::Left((output, _)) => output?,
        Either::Right(_) => {
            return Err(SourceFailure::TimedOut {
                command: command.clone(),
            });
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let details = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_owned())
            .unwrap_or_else(|| match output.status.code() {
                Some(code) => format!("exited with status {code}"),
                None => "terminated by a signal".to_owned(),
            });
        return Err(SourceFailure::Failed { command, details });
    }

    if output.stdout.len() > MAX_OUTPUT_BYTES {
        return Err(SourceFailure::TooMuchOutput {
            command,
            bytes: output.stdout.len(),
        });
    }

    Ok(entries_from_stdout(&output.stdout))
}

fn entries_from_stdout(stdout: &[u8]) -> Vec<SharedString> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(SharedString::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::os::unix::process::ExitStatusExt as _;

    struct TestRunner {
        output: Result<Output, String>,
        delay: Option<Duration>,
        executor: BackgroundExecutor,
    }

    #[async_trait]
    impl SourceRunner for TestRunner {
        async fn run(
            &self,
            _command: &str,
            _args: &[String],
            _cwd: &Path,
            _env: &HashMap<String, String>,
        ) -> Result<Output, std::io::Error> {
            if let Some(delay) = self.delay {
                self.executor.timer(delay).await;
            }
            match &self.output {
                Ok(output) => Ok(output.clone()),
                Err(message) => Err(std::io::Error::other(message.clone())),
            }
        }
    }

    fn succeeding(stdout: &str) -> Result<Output, String> {
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn failing(stderr: &str) -> Result<Output, String> {
        Ok(Output {
            // Wait status 256 is exit code 1.
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    fn source() -> Source {
        Source::Command {
            command: "git".into(),
            args: Vec::new(),
        }
    }

    async fn run(
        runner: TestRunner,
        executor: BackgroundExecutor,
    ) -> Result<Vec<SharedString>, SourceFailure> {
        run_source(
            Arc::new(runner),
            source(),
            Arc::from(Path::new("/project")),
            HashMap::default(),
            executor,
        )
        .await
    }

    #[gpui::test]
    async fn splits_stdout_into_trimmed_non_empty_entries(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let entries = run(
            TestRunner {
                output: succeeding("main\n  feature/one  \n\nfeature/two\n"),
                delay: None,
                executor: executor.clone(),
            },
            executor,
        )
        .await
        .expect("source succeeded");

        assert_eq!(entries, vec!["main", "feature/one", "feature/two"]);
    }

    #[gpui::test]
    async fn reports_a_command_that_could_not_be_spawned(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let failure = run(
            TestRunner {
                output: Err("No such file or directory".into()),
                delay: None,
                executor: executor.clone(),
            },
            executor,
        )
        .await
        .expect_err("source failed");

        assert!(matches!(failure, SourceFailure::CouldNotSpawn { .. }));
        assert!(failure.to_string().contains("No such file or directory"));
    }

    #[gpui::test]
    async fn reports_a_non_zero_exit_with_its_first_stderr_line(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let failure = run(
            TestRunner {
                output: failing("fatal: not a git repository\nmore detail\n"),
                delay: None,
                executor: executor.clone(),
            },
            executor,
        )
        .await
        .expect_err("source failed");

        assert_eq!(
            failure,
            SourceFailure::Failed {
                command: "git".into(),
                details: "fatal: not a git repository".into(),
            }
        );
    }

    #[gpui::test]
    async fn times_out_a_source_that_never_finishes(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let task = cx.background_executor.spawn(run(
            TestRunner {
                output: succeeding("never delivered"),
                delay: Some(SOURCE_TIMEOUT * 10),
                executor: executor.clone(),
            },
            executor.clone(),
        ));

        executor.advance_clock(SOURCE_TIMEOUT * 2);
        let failure = task.await.expect_err("source timed out");

        assert!(matches!(failure, SourceFailure::TimedOut { .. }));
    }

    #[gpui::test]
    async fn refuses_output_larger_than_the_cap(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let oversized = "x".repeat(MAX_OUTPUT_BYTES + 1);
        let failure = run(
            TestRunner {
                output: succeeding(&oversized),
                delay: None,
                executor: executor.clone(),
            },
            executor,
        )
        .await
        .expect_err("source failed");

        assert!(matches!(failure, SourceFailure::TooMuchOutput { .. }));
        assert!(failure.to_string().contains("streaming"));
    }
}
