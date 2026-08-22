use async_trait::async_trait;
use collections::HashMap;
use futures::{
    AsyncBufReadExt as _, AsyncReadExt as _, Stream, StreamExt as _,
    channel::mpsc,
    future::{self, Either},
    io::BufReader,
    stream,
};
use gpui::{BackgroundExecutor, SharedString};
use std::{fmt, path::Path, pin::Pin, sync::Arc, time::Duration};
use util::command::Stdio;

/// How long a Source may produce nothing before it is abandoned. Once it has
/// produced something it is never abandoned for slowness.
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(5);

/// The most Entries a Source may produce; beyond this the Source is dropped.
pub const MAX_ENTRIES: usize = 100_000;

/// Entries are accumulated for this long before being handed on, so a fast
/// Source does not trigger a re-match per line.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq)]
pub enum SourceEvent {
    Line(String),
    Exited {
        /// `None` when the process was terminated by a signal.
        code: Option<i32>,
        stderr: String,
    },
}

pub type SourceStream = Pin<Box<dyn Stream<Item = SourceEvent> + Send>>;

/// The seam between a Source declaration and a real process, so the timeout,
/// cap, and flush policy in [`run_source`] stay under test.
#[async_trait]
pub trait SourceRunner: Send + Sync {
    async fn spawn(
        &self,
        command: &str,
        args: &[String],
        cwd: &Path,
        env: &HashMap<String, String>,
    ) -> Result<SourceStream, std::io::Error>;
}

pub struct DefaultSourceRunner;

struct GlobalSourceRunner(Arc<dyn SourceRunner>);

impl gpui::Global for GlobalSourceRunner {}

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
    async fn spawn(
        &self,
        command: &str,
        args: &[String],
        cwd: &Path,
        env: &HashMap<String, String>,
    ) -> Result<SourceStream, std::io::Error> {
        let mut child = util::command::new_command(command)
            .args(args)
            .current_dir(cwd)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::other("the child process was spawned without a stdout pipe")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            std::io::Error::other("the child process was spawned without a stderr pipe")
        })?;

        let lines = BufReader::new(stdout).lines();

        Ok(Box::pin(stream::unfold(
            Some((lines, child, stderr)),
            move |state| async move {
                let (mut lines, mut child, mut stderr) = state?;
                // Drain stdout to EOF before waiting on the process, so a child
                // that fills its stdout pipe can still make progress.
                loop {
                    match lines.next().await {
                        Some(Ok(line)) => {
                            return Some((SourceEvent::Line(line), Some((lines, child, stderr))));
                        }
                        // Not UTF-8 says nothing about the rest of the stream;
                        // skip rather than end early.
                        Some(Err(_)) => continue,
                        None => break,
                    }
                }

                let mut captured = String::new();
                stderr.read_to_string(&mut captured).await.ok();
                let code = child.status().await.ok().and_then(|status| status.code());
                Some((
                    SourceEvent::Exited {
                        code,
                        stderr: captured,
                    },
                    None,
                ))
            },
        )))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceFailure {
    CouldNotSpawn { command: String, reason: String },
    Failed { command: String, details: String },
    TimedOut { command: String },
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
                "`{command}` produced nothing within {} seconds",
                SOURCE_TIMEOUT.as_secs()
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceUpdate {
    Entries(Vec<SharedString>),
    Finished { truncated: bool },
    Failed(SourceFailure),
}

/// Runs a command, coalescing output over [`FLUSH_INTERVAL`].
pub async fn run_source(
    runner: Arc<dyn SourceRunner>,
    command: String,
    args: Vec<String>,
    cwd: Arc<Path>,
    env: HashMap<String, String>,
    executor: BackgroundExecutor,
    updates: mpsc::UnboundedSender<SourceUpdate>,
) {
    let mut events = match runner.spawn(&command, &args, cwd.as_ref(), &env).await {
        Ok(events) => events,
        Err(error) => {
            updates
                .unbounded_send(SourceUpdate::Failed(SourceFailure::CouldNotSpawn {
                    command,
                    reason: error.to_string(),
                }))
                .ok();
            return;
        }
    };

    let first = {
        let next = events.next();
        futures::pin_mut!(next);
        match future::select(next, Box::pin(executor.timer(SOURCE_TIMEOUT))).await {
            Either::Left((event, _)) => event,
            Either::Right(_) => {
                updates
                    .unbounded_send(SourceUpdate::Failed(SourceFailure::TimedOut { command }))
                    .ok();
                return;
            }
        }
    };

    let mut pending = Vec::new();
    let mut delivered = 0usize;
    let mut flush = Box::pin(executor.timer(FLUSH_INTERVAL));
    let mut event = first;

    loop {
        match event {
            Some(SourceEvent::Line(line)) => {
                let line = line.trim();
                if !line.is_empty() {
                    pending.push(SharedString::from(line.to_owned()));
                    if delivered + pending.len() >= MAX_ENTRIES {
                        send_entries(&updates, &mut pending);
                        // Dropping the stream kills the child.
                        drop(events);
                        updates
                            .unbounded_send(SourceUpdate::Finished { truncated: true })
                            .ok();
                        return;
                    }
                }
            }
            Some(SourceEvent::Exited { code, stderr }) => {
                send_entries(&updates, &mut pending);
                let update = match code {
                    Some(0) => SourceUpdate::Finished { truncated: false },
                    code => SourceUpdate::Failed(SourceFailure::Failed {
                        command,
                        details: exit_details(code, &stderr),
                    }),
                };
                updates.unbounded_send(update).ok();
                return;
            }
            // The runner ended the stream without reporting an exit. Treat what
            // arrived as everything there is.
            None => {
                send_entries(&updates, &mut pending);
                updates
                    .unbounded_send(SourceUpdate::Finished { truncated: false })
                    .ok();
                return;
            }
        }

        let next = events.next();
        futures::pin_mut!(next);
        match future::select(next, &mut flush).await {
            Either::Left((next_event, _)) => event = next_event,
            Either::Right(_) => {
                delivered += pending.len();
                send_entries(&updates, &mut pending);
                if updates.is_closed() {
                    return;
                }
                flush = Box::pin(executor.timer(FLUSH_INTERVAL));
                event = events.next().await;
            }
        }
    }
}

fn send_entries(updates: &mpsc::UnboundedSender<SourceUpdate>, pending: &mut Vec<SharedString>) {
    if !pending.is_empty() {
        updates
            .unbounded_send(SourceUpdate::Entries(std::mem::take(pending)))
            .ok();
    }
}

fn exit_details(code: Option<i32>, stderr: &str) -> String {
    stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_owned())
        .unwrap_or_else(|| match code {
            Some(code) => format!("exited with status {code}"),
            None => "terminated by a signal".to_owned(),
        })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    #[derive(Debug, Clone)]
    pub enum Step {
        Line(&'static str),
        Wait(Duration),
        Exit {
            code: Option<i32>,
            stderr: &'static str,
        },
    }

    pub fn exit_ok() -> Step {
        Step::Exit {
            code: Some(0),
            stderr: "",
        }
    }

    pub fn exit_with(stderr: &'static str) -> Step {
        Step::Exit {
            code: Some(1),
            stderr,
        }
    }

    /// Yields `lines` and then exits cleanly, without delay.
    pub fn emitting(lines: &[&'static str]) -> Vec<Step> {
        lines
            .iter()
            .map(|line| Step::Line(line))
            .chain([exit_ok()])
            .collect()
    }

    pub struct ScriptedRunner {
        per_spawn: std::sync::Mutex<Vec<Vec<Step>>>,
        spawn_error: Option<String>,
        executor: BackgroundExecutor,
        tail: Vec<Step>,
        spawned_args: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl ScriptedRunner {
        pub fn new(steps: Vec<Step>, executor: BackgroundExecutor) -> Self {
            Self::per_spawn(vec![steps], executor)
        }

        pub fn per_spawn(per_spawn: Vec<Vec<Step>>, executor: BackgroundExecutor) -> Self {
            Self {
                per_spawn: std::sync::Mutex::new(per_spawn),
                spawn_error: None,
                executor,
                tail: vec![exit_ok()],
                spawned_args: std::sync::Mutex::new(Vec::new()),
            }
        }

        pub fn failing_to_spawn(reason: &str, executor: BackgroundExecutor) -> Self {
            Self {
                per_spawn: std::sync::Mutex::new(vec![Vec::new()]),
                spawn_error: Some(reason.to_owned()),
                executor,
                tail: vec![exit_ok()],
                spawned_args: std::sync::Mutex::new(Vec::new()),
            }
        }

        pub fn spawned_args(&self) -> Vec<Vec<String>> {
            self.spawned_args.lock().expect("unpoisoned").clone()
        }
    }

    #[async_trait]
    impl SourceRunner for ScriptedRunner {
        async fn spawn(
            &self,
            _command: &str,
            args: &[String],
            _cwd: &Path,
            _env: &HashMap<String, String>,
        ) -> Result<SourceStream, std::io::Error> {
            if let Some(reason) = &self.spawn_error {
                return Err(std::io::Error::other(reason.clone()));
            }

            self.spawned_args
                .lock()
                .expect("unpoisoned")
                .push(args.to_vec());

            let steps = {
                let mut queue = self.per_spawn.lock().expect("unpoisoned");
                if !queue.is_empty() {
                    queue.remove(0)
                } else {
                    self.tail.clone()
                }
            };

            let executor = self.executor.clone();
            Ok(Box::pin(stream::unfold(
                steps.into_iter(),
                move |mut steps| {
                    let executor = executor.clone();
                    async move {
                        loop {
                            match steps.next()? {
                                Step::Wait(duration) => executor.timer(duration).await,
                                Step::Line(line) => {
                                    return Some((SourceEvent::Line(line.to_owned()), steps));
                                }
                                Step::Exit { code, stderr } => {
                                    return Some((
                                        SourceEvent::Exited {
                                            code,
                                            stderr: stderr.to_owned(),
                                        },
                                        steps,
                                    ));
                                }
                            }
                        }
                    }
                },
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::*, *};
    use crate::config::Source;
    use gpui::TestAppContext;

    fn source() -> Source {
        Source::Command {
            command: "git".into(),
            args: Vec::new(),
        }
    }

    async fn collect(
        runner: impl SourceRunner + 'static,
        cx: &mut TestAppContext,
    ) -> Vec<SourceUpdate> {
        let executor = cx.executor();
        let (sender, receiver) = mpsc::unbounded();
        let Source::Command { command, args } = source() else {
            unreachable!()
        };
        let task = cx.background_executor.spawn(run_source(
            Arc::new(runner),
            command,
            args,
            Arc::from(Path::new("/project")),
            HashMap::default(),
            executor.clone(),
            sender,
        ));

        // Long enough for every flush interval and timeout in these tests.
        executor.advance_clock(SOURCE_TIMEOUT * 10);
        task.await;
        receiver.collect().await
    }

    fn entries(updates: &[SourceUpdate]) -> Vec<String> {
        updates
            .iter()
            .filter_map(|update| match update {
                SourceUpdate::Entries(entries) => Some(entries),
                _ => None,
            })
            .flatten()
            .map(|entry| entry.to_string())
            .collect()
    }

    #[gpui::test]
    async fn reports_trimmed_non_empty_lines_as_entries(cx: &mut TestAppContext) {
        let updates = collect(
            ScriptedRunner::new(
                emitting(&["main", "  feature/one  ", "", "feature/two"]),
                cx.executor(),
            ),
            cx,
        )
        .await;

        assert_eq!(
            entries(&updates),
            vec!["main", "feature/one", "feature/two"]
        );
        assert_eq!(
            updates.last(),
            Some(&SourceUpdate::Finished { truncated: false })
        );
    }

    #[gpui::test]
    async fn reports_a_command_that_could_not_be_spawned(cx: &mut TestAppContext) {
        let updates = collect(
            ScriptedRunner::failing_to_spawn("No such file or directory", cx.executor()),
            cx,
        )
        .await;

        let [SourceUpdate::Failed(failure)] = updates.as_slice() else {
            panic!("expected a single failure, got {updates:?}");
        };
        assert!(matches!(failure, SourceFailure::CouldNotSpawn { .. }));
        assert!(failure.to_string().contains("No such file or directory"));
    }

    #[gpui::test]
    async fn reports_a_non_zero_exit_with_its_first_stderr_line(cx: &mut TestAppContext) {
        let updates = collect(
            ScriptedRunner::new(
                vec![exit_with("fatal: not a git repository\nmore detail\n")],
                cx.executor(),
            ),
            cx,
        )
        .await;

        assert_eq!(
            updates.last(),
            Some(&SourceUpdate::Failed(SourceFailure::Failed {
                command: "git".into(),
                details: "fatal: not a git repository".into(),
            }))
        );
    }

    #[gpui::test]
    async fn keeps_entries_produced_before_a_failure(cx: &mut TestAppContext) {
        let updates = collect(
            ScriptedRunner::new(
                vec![
                    Step::Line("a.rs"),
                    Step::Line("b.rs"),
                    exit_with("fatal: interrupted"),
                ],
                cx.executor(),
            ),
            cx,
        )
        .await;

        assert_eq!(entries(&updates), vec!["a.rs", "b.rs"]);
        assert!(matches!(
            updates.last(),
            Some(SourceUpdate::Failed(SourceFailure::Failed { .. }))
        ));
    }

    #[gpui::test]
    async fn abandons_a_source_that_produces_nothing(cx: &mut TestAppContext) {
        let updates = collect(
            ScriptedRunner::new(
                vec![Step::Wait(SOURCE_TIMEOUT * 100), Step::Line("too late")],
                cx.executor(),
            ),
            cx,
        )
        .await;

        assert_eq!(
            updates,
            vec![SourceUpdate::Failed(SourceFailure::TimedOut {
                command: "git".into()
            })]
        );
    }

    #[gpui::test]
    async fn does_not_abandon_a_slow_source_that_is_still_producing(cx: &mut TestAppContext) {
        let updates = collect(
            ScriptedRunner::new(
                vec![
                    Step::Line("first"),
                    Step::Wait(SOURCE_TIMEOUT * 3),
                    Step::Line("second"),
                    exit_ok(),
                ],
                cx.executor(),
            ),
            cx,
        )
        .await;

        assert_eq!(entries(&updates), vec!["first", "second"]);
        assert_eq!(
            updates.last(),
            Some(&SourceUpdate::Finished { truncated: false })
        );
    }

    #[gpui::test]
    async fn delivers_entries_in_batches_as_they_arrive(cx: &mut TestAppContext) {
        let updates = collect(
            ScriptedRunner::new(
                vec![
                    Step::Line("first"),
                    Step::Wait(FLUSH_INTERVAL * 3),
                    Step::Line("second"),
                    exit_ok(),
                ],
                cx.executor(),
            ),
            cx,
        )
        .await;

        let batches: Vec<_> = updates
            .iter()
            .filter(|update| matches!(update, SourceUpdate::Entries(_)))
            .collect();
        assert_eq!(batches.len(), 2, "expected two batches, got {updates:?}");
        assert_eq!(entries(&updates), vec!["first", "second"]);
    }

    #[gpui::test]
    async fn stops_at_the_entry_cap_and_reports_truncation(cx: &mut TestAppContext) {
        let steps = std::iter::repeat_n(Step::Line("entry"), MAX_ENTRIES + 10)
            .chain([exit_ok()])
            .collect();
        let updates = collect(ScriptedRunner::new(steps, cx.executor()), cx).await;

        assert_eq!(entries(&updates).len(), MAX_ENTRIES);
        assert_eq!(
            updates.last(),
            Some(&SourceUpdate::Finished { truncated: true })
        );
    }
}
