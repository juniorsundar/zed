use crate::{
    config::{FinderConfig, Preview, Source, substitute_query},
    outcome::resolve_outcome_path,
    positioned_preview::{PendingPosition, new_pending_position, positioned_editor_preview},
    remote::resolve_command,
    source::{MAX_ENTRIES, SourceFailure, SourceUpdate, run_source, source_runner},
};
use crate::outcome::apply_outcome;
use collections::HashMap;
use futures::{StreamExt as _, channel::mpsc};
use fuzzy_nucleo::{Case, LengthPenalty, StringMatch, StringMatchCandidate};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    HighlightStyle, IntoElement, Render, SharedString, Task, WeakEntity, Window,
};
use picker::{HighlightedTextBuilder, Picker, PickerDelegate, PreviewUpdate};
use project::Project;
use std::{path::Path, sync::Arc, time::Duration};
use ui::{Color, HighlightedLabel, Icon, IconName, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt as _;
use workspace::{ModalView, Workspace};

const MAX_MATCHES: usize = 1000;

pub struct FinderPicker {
    picker: Entity<Picker<FinderDelegate>>,
}

impl FinderPicker {
    pub fn new(
        config: Arc<FinderConfig>,
        cwd: Arc<Path>,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let previewed = config.preview.is_some();
        let is_query_driven = config.source.is_query_driven();
        let pending_position = new_pending_position();
        let delegate = FinderDelegate::new(
            cx.entity().downgrade(),
            workspace,
            config,
            cwd,
            project.clone(),
            pending_position.clone(),
        );
        let picker = cx.new(|cx| {
            let mut picker = if previewed {
                let preview =
                    positioned_editor_preview(project.clone(), pending_position, window, cx);
                Picker::uniform_list_with_preview(delegate, preview, window, cx)
            } else {
                Picker::uniform_list(delegate, window, cx)
            };
            // A query-driven Source waits for the first Query; the picker's
            // opening `update_matches("")` suppresses it.
            if !is_query_driven {
                picker.delegate.source_task = picker.delegate.spawn_source(project, window, cx);
            }
            picker
        });
        Self { picker }
    }

    pub fn picker(&self) -> &Entity<Picker<FinderDelegate>> {
        &self.picker
    }
}

impl Render for FinderPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // No width: the Picker sizes itself, growing to its larger
        // "telescope" shape when a Preview is showing.
        v_flex().child(self.picker.clone())
    }
}

impl Focusable for FinderPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for FinderPicker {}
impl ModalView for FinderPicker {}

/// A Preview that shows text instead of a file, with `subject` emphasized.
fn message_preview(subject: &str, reason: Option<&str>, cx: &App) -> PreviewUpdate {
    let mut message = HighlightedTextBuilder::default();
    message.push_styled(
        subject,
        HighlightStyle {
            color: Some(cx.theme().colors().text_accent),
            ..Default::default()
        },
    );
    if let Some(reason) = reason {
        message.push_plain(format!(" {reason}."));
    }
    PreviewUpdate::message(message.build())
}

/// How long after the user stops typing before a query-driven Source is
/// re-spawned.
pub const QUERY_DEBOUNCE: Duration = Duration::from_millis(150);

/// Bumped per run; batches carrying a stale generation are dropped.
type Generation = u64;

/// Surfuses a Source that could not be started. The send only fails once the Picker's update
/// channel has closed.
fn send_could_not_spawn(
    sender: &mpsc::UnboundedSender<SourceUpdate>,
    command: &str,
    reason: impl std::fmt::Display,
) {
    if let Err(_) = sender.unbounded_send(SourceUpdate::Failed(SourceFailure::CouldNotSpawn {
        command: command.to_owned(),
        reason: reason.to_string(),
    })) {
        // The picker is gone; there is nobody left to show the failure.
    }
}

#[derive(Default)]
struct SourceState {
    running: bool,
    truncated: bool,
    failure: Option<SharedString>,
}

pub struct FinderDelegate {
    finder: WeakEntity<FinderPicker>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    config: Arc<FinderConfig>,
    cwd: Arc<Path>,
    entries: Arc<Vec<StringMatchCandidate>>,
    matches: Vec<StringMatch>,
    selected_index: usize,
    state: SourceState,
    keep_selected_entry: bool,
    source_task: Task<()>,
    query: Option<QueryState>,
    pub(crate) pending_position: PendingPosition,
}

#[derive(Default)]
struct QueryState {
    generation: Generation,
    pending_query: Option<String>,
    force_wake: Option<futures::channel::mpsc::UnboundedSender<()>>,
}

impl FinderDelegate {
    fn new(
        finder: WeakEntity<FinderPicker>,
        workspace: WeakEntity<Workspace>,
        config: Arc<FinderConfig>,
        cwd: Arc<Path>,
        project: Entity<Project>,
        pending_position: PendingPosition,
    ) -> Self {
        let is_query_driven = config.source.is_query_driven();
        Self {
            finder,
            workspace,
            project,
            config,
            cwd,
            entries: Arc::new(Vec::new()),
            matches: Vec::new(),
            selected_index: 0,
            state: SourceState {
                running: true,
                ..SourceState::default()
            },
            keep_selected_entry: false,
            source_task: Task::ready(()),
            query: is_query_driven.then(QueryState::default),
            pending_position,
        }
    }

    fn spawn_source(
        &self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let runner = source_runner(cx);
        let cwd = self.cwd.clone();
        let run_on = self.config.run_on;
        let executor = cx.background_executor().clone();
        let remote_transport = project.read(cx).remote_client();
        let (command, args, success_exit_codes) = match &self.config.source {
            Source::Command {
                command,
                args,
                success_exit_codes,
            }
            | Source::Query {
                command,
                args,
                success_exit_codes,
            } => (command.clone(), args.clone(), success_exit_codes.clone()),
        };

        let environment = project.update(cx, |project, cx| {
            let worktree = project.visible_worktrees(cx).next()?;
            Some(project.environment().update(cx, |environment, cx| {
                environment.worktree_environment(worktree, cx)
            }))
        });

        cx.spawn_in(window, async move |picker, cx| {
            let environment = match environment {
                Some(environment) => environment.await.unwrap_or_default(),
                None => HashMap::default(),
            };

            let (sender, mut updates) = mpsc::unbounded();
            let source_task = match resolve_command(
                remote_transport.as_ref(),
                run_on,
                command.clone(),
                args.clone(),
                &cwd,
                environment,
                cx,
            ) {
                Ok(resolved) => Some(cx.background_spawn(run_source(
                    runner,
                    command,
                    resolved.command,
                    resolved.args,
                    resolved.cwd.map(Arc::from),
                    success_exit_codes,
                    resolved.env,
                    executor,
                    sender,
                ))),
                Err(error) => {
                    send_could_not_spawn(&sender, &command, &error);
                    None
                }
            };
            let _source = source_task;

            while let Some(update) = updates.next().await {
                let applied = picker.update_in(cx, |picker, window, cx| {
                    picker.delegate.apply(update);
                    let query = picker.query(cx);
                    picker.update_matches(query, window, cx);
                });
                if applied.is_err() {
                    return;
                }
            }
        })
    }

    /// Debounces the Query, then spawns one run of the Source for it, tagged
    /// with the current generation. Dropping the returned Task cancels the
    /// debounce and kills the child.
    fn update_query_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        if query.is_empty() {
            self.entries = Arc::new(Vec::new());
            self.matches.clear();
            self.selected_index = 0;
            self.state = SourceState::default();
            if let Some(query_state) = &mut self.query {
                query_state.generation = query_state.generation.wrapping_add(1);
                query_state.pending_query = None;
            }
            return Task::ready(());
        }

        let project = self.project.clone();
        let cwd = self.cwd.clone();
        let executor = cx.background_executor().clone();
        let config = self.config.clone();
        // Enter short-circuits the debounce through this channel.
        let (force_tx, force_rx) = mpsc::unbounded::<()>();
        let generation = {
            let query_state = self.query.as_mut().expect("query-driven only");
            query_state.generation = query_state.generation.wrapping_add(1);
            query_state.pending_query = Some(query.clone());
            query_state.force_wake = Some(force_tx);
            query_state.generation
        };

        // The list is always the answer to the current Query or empty, never a
        // stale answer to the previous one.
        self.entries = Arc::new(Vec::new());
        self.matches.clear();
        self.selected_index = 0;
        self.state = SourceState {
            running: true,
            ..SourceState::default()
        };
        cx.notify();

        let command = config.source.command().to_owned();
        let args = substitute_query(config.source.args(), &query);
        let success_exit_codes = config.source.success_exit_codes().to_vec();
        let runner = source_runner(cx);
        let run_on = config.run_on;
        let remote_transport = project.read(cx).remote_client();

        let environment = project.update(cx, |project, cx| {
            let worktree = project.visible_worktrees(cx).next()?;
            Some(project.environment().update(cx, |environment, cx| {
                environment.worktree_environment(worktree, cx)
            }))
        });

        cx.spawn_in(window, async move |picker, cx| {
            // Debounce, but yield immediately if Enter fires `force_wake`.
            let mut force_rx = force_rx;
            let timer = cx.background_executor().timer(QUERY_DEBOUNCE);
            futures::pin_mut!(timer);
            let woke = futures::future::select(timer, force_rx.next()).await;
            let _ = woke;
            // A newer Query replaced this task and cancelled us; bail anyway.
            let superseded = picker
                .read_with(cx, |picker, _| {
                    picker
                        .delegate
                        .query
                        .as_ref()
                        .map(|state| state.generation != generation)
                        .unwrap_or(true)
                })
                .unwrap_or(true);
            if superseded {
                return;
            }

            let environment = match environment {
                Some(environment) => environment.await.unwrap_or_default(),
                None => HashMap::default(),
            };

            let (sender, mut updates) = mpsc::unbounded();
            // Dropping the task kills the child; hold it until the picker is
            // dropped rather than scoping it to the match arm below.
            let _source = match resolve_command(
                remote_transport.as_ref(),
                run_on,
                command.clone(),
                args.clone(),
                &cwd,
                environment,
                cx,
            ) {
                Ok(resolved) => Some(cx.background_spawn(run_source(
                    runner,
                    command,
                    resolved.command,
                    resolved.args,
                    resolved.cwd.map(Arc::from),
                    success_exit_codes,
                    resolved.env,
                    executor,
                    sender,
                ))),
                Err(error) => {
                    send_could_not_spawn(&sender, &command, &error);
                    None
                }
            };

            while let Some(update) = updates.next().await {
                let applied = picker.update_in(cx, |picker, _window, cx| {
                    picker.delegate.apply_query(generation, update);
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }
        })
    }

    /// Drops batches from a superseded run; the Source owns filtering.
    fn apply_query(&mut self, generation: Generation, update: SourceUpdate) {
        let query_state = self.query.as_mut().expect("query-driven only");
        if generation != query_state.generation {
            return;
        }
        match update {
            SourceUpdate::Entries(entries) => {
                let first_id = self.entries.len();
                Arc::make_mut(&mut self.entries).extend(entries.iter().enumerate().map(
                    |(offset, entry)| StringMatchCandidate::new(first_id + offset, entry.clone()),
                ));
                self.matches
                    .extend(entries.into_iter().map(|entry| StringMatch {
                        // Unused for rendering; no highlights on query matches.
                        candidate_id: 0,
                        score: 0.,
                        positions: Vec::new(),
                        string: entry,
                    }));
            }
            SourceUpdate::Finished { truncated } => {
                self.state.running = false;
                self.state.truncated = truncated;
            }
            SourceUpdate::Failed(failure) => {
                self.state.running = false;
                self.state.failure = Some(failure.to_string().into());
            }
        }
    }

    fn apply(&mut self, update: SourceUpdate) {
        match update {
            SourceUpdate::Entries(entries) => {
                let first_id = self.entries.len();
                Arc::make_mut(&mut self.entries).extend(
                    entries
                        .into_iter()
                        .enumerate()
                        .map(|(offset, entry)| StringMatchCandidate::new(first_id + offset, entry)),
                );
            }
            SourceUpdate::Finished { truncated } => {
                self.state.running = false;
                self.state.truncated = truncated;
            }
            SourceUpdate::Failed(failure) => {
                self.state.running = false;
                self.state.failure = Some(failure.to_string().into());
            }
        }
        self.keep_selected_entry = true;
    }

    /// A failure that arrived after usable Entries, or a Source cut short at
    /// the cap: things the list itself cannot carry.
    fn footer_message(&self) -> Option<SharedString> {
        if !self.entries.is_empty()
            && let Some(failure) = &self.state.failure
        {
            return Some(failure.clone());
        }
        if self.state.truncated {
            return Some(format!("Showing the first {MAX_ENTRIES} entries").into());
        }
        None
    }

    fn status_message(&self) -> Option<SharedString> {
        if self.entries.is_empty()
            && let Some(failure) = &self.state.failure
        {
            return Some(failure.clone());
        }
        self.state.running.then(|| "Running…".into())
    }
}

impl PickerDelegate for FinderDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "finder"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        self.config.placeholder.as_ref().into()
    }

    /// Doubles as the Source's status line: the picker opens before the
    /// Source has produced anything, so an empty list must say why.
    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some(self.status_message().unwrap_or_else(|| "No matches".into()))
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        if self.query.is_some() {
            return self.update_query_matches(query, window, cx);
        }

        let entries = self.entries.clone();
        let selected_entry = std::mem::take(&mut self.keep_selected_entry)
            .then(|| self.matches.get(self.selected_index))
            .flatten()
            .map(|entry| entry.string.clone());

        cx.spawn(async move |picker, cx| {
            let matches = cx
                .background_spawn(async move {
                    if query.is_empty() {
                        entries
                            .iter()
                            .take(MAX_MATCHES)
                            .map(|candidate| StringMatch {
                                candidate_id: candidate.id,
                                score: 0.,
                                positions: Vec::new(),
                                string: candidate.string.clone(),
                            })
                            .collect()
                    } else {
                        fuzzy_nucleo::match_strings(
                            entries.as_slice(),
                            &query,
                            Case::Smart,
                            LengthPenalty::On,
                            MAX_MATCHES,
                        )
                    }
                })
                .await;

            picker
                .update(cx, |picker, cx| {
                    let delegate = &mut picker.delegate;
                    delegate.selected_index = selected_entry
                        .and_then(|selected| {
                            matches.iter().position(|entry| entry.string == selected)
                        })
                        .unwrap_or(0);
                    delegate.matches = matches;
                    cx.notify();
                })
                .log_err();
        })
    }

    fn finalize_update_matches(
        &mut self,
        _query: String,
        _duration: Duration,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> bool {
        // The one-shot path has already produced its matches synchronously, so
        // there is nothing to finalize.
        let Some(query_state) = &mut self.query else {
            return true;
        };

        // Short-circuit any pending debounce so the run spawns now rather than
        // after the debounce elapses.
        if let Some(wake) = query_state.force_wake.take() {
            wake.unbounded_send(()).ok();
        }

        // Defer: the pending debounce+drain task completes when the run settles,
        // and the picker then fires the deferred confirm via `confirm_on_update`.
        // If there is no pending run at all, there is nothing to wait on.
        query_state.pending_query.is_none()
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(entry) = self
            .matches
            .get(self.selected_index)
            .map(|entry| entry.string.clone())
        else {
            return;
        };

        let outcome = self.config.outcome.clone();
        let delimiter = self.config.delimiter.clone();
        let run_on = self.config.run_on;
        let workspace = self.workspace.clone();
        let project = self.project.clone();
        let cwd = self.cwd.clone();

        // Dismiss first so a dispatched action lands on the focus the modal was
        // covering rather than on the modal itself.
        self.dismissed(window, cx);
        cx.defer_in(window, move |_, window, cx| {
            apply_outcome(
                &outcome,
                &entry,
                cwd.as_ref(),
                delimiter.as_deref(),
                run_on,
                &workspace,
                &project,
                window,
                cx,
            );
        });
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.finder
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn try_get_preview_data_for_match(&self, cx: &App) -> Option<PreviewUpdate> {
        self.pending_position.lock().take();
        let Some(Preview::Path) = self.config.preview else {
            return None;
        };
        let entry = self.matches.get(self.selected_index)?;

        // Resolved via the Outcome so the Preview shows what Confirm opens.
        let resolved = resolve_outcome_path(
            &self.config.outcome,
            &entry.string,
            &self.cwd,
            self.config.delimiter.as_deref(),
        )?;
        let target = match resolved {
            Ok(target) => target,
            Err(error) => return Some(message_preview(&error, None, cx)),
        };
        let path = target.path.clone();

        // Only a path inside the project can be classified here, because this
        // runs synchronously and the filesystem cannot be consulted. A path
        // outside every worktree is previewed optimistically.
        let project = self.project.read(cx);
        if let Some(project_path) = project.project_path_for_absolute_path(&path, cx) {
            let reason = match project.entry_for_path(&project_path, cx) {
                Some(entry) if entry.is_dir() => Some("is a directory"),
                Some(_) => None,
                None => Some("no longer exists"),
            };
            if let Some(reason) = reason {
                return Some(message_preview(
                    &path.display().to_string(),
                    Some(reason),
                    cx,
                ));
            }
        }

        if target.row.is_some() {
            *self.pending_position.lock() = Some(target);
        }
        Some(PreviewUpdate::from_path(path))
    }

    fn render_footer(
        &self,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<AnyElement> {
        let message = self.footer_message()?;
        Some(
            h_flex()
                .w_full()
                .gap_1p5()
                .p_2()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Icon::new(IconName::Warning)
                        .size(IconSize::Small)
                        .color(Color::Warning),
                )
                .child(
                    Label::new(message)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element(),
        )
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry = self.matches.get(ix)?;
        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(HighlightedLabel::new(
                    entry.string.clone(),
                    entry.positions.clone(),
                )),
        )
    }
}

#[cfg(test)]
impl FinderDelegate {
    pub fn matched_entries(&self) -> Vec<SharedString> {
        self.matches
            .iter()
            .map(|entry| entry.string.clone())
            .collect()
    }

    pub fn status_text(&self) -> Option<SharedString> {
        self.status_message()
    }

    pub fn footer_text(&self) -> Option<SharedString> {
        self.footer_message()
    }

    pub fn preview_target(&self, cx: &App) -> Option<PreviewUpdate> {
        self.try_get_preview_data_for_match(cx)
    }
}
