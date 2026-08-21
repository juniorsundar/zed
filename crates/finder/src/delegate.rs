use crate::{
    config::FinderConfig,
    outcome::apply_outcome,
    source::{run_source, source_runner},
};
use collections::HashMap;
use fuzzy_nucleo::{Case, LengthPenalty, StringMatch, StringMatchCandidate};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    SharedString, Task, WeakEntity, Window,
};
use picker::{Picker, PickerDelegate};
use project::Project;
use std::{path::Path, sync::Arc};
use ui::{HighlightedLabel, ListItem, ListItemSpacing, prelude::*};
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
        let delegate =
            FinderDelegate::new(cx.entity().downgrade(), workspace, config, cwd);
        let picker = cx.new(|cx| {
            let mut picker = Picker::uniform_list(delegate, window, cx);
            picker.delegate.source_task = picker.delegate.spawn_source(project, window, cx);
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
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

impl Focusable for FinderPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for FinderPicker {}
impl ModalView for FinderPicker {}

/// Whether the Source has produced Entries yet, and why it did not.
enum SourceState {
    Running,
    Ready,
    Failed(SharedString),
}

pub struct FinderDelegate {
    finder: WeakEntity<FinderPicker>,
    workspace: WeakEntity<Workspace>,
    config: Arc<FinderConfig>,
    cwd: Arc<Path>,
    entries: Arc<Vec<StringMatchCandidate>>,
    matches: Vec<StringMatch>,
    selected_index: usize,
    state: SourceState,
    source_task: Task<()>,
}

impl FinderDelegate {
    fn new(
        finder: WeakEntity<FinderPicker>,
        workspace: WeakEntity<Workspace>,
        config: Arc<FinderConfig>,
        cwd: Arc<Path>,
    ) -> Self {
        Self {
            finder,
            workspace,
            config,
            cwd,
            entries: Arc::new(Vec::new()),
            matches: Vec::new(),
            selected_index: 0,
            state: SourceState::Running,
            source_task: Task::ready(()),
        }
    }

    /// Runs the Source and installs its Entries. The picker is already on
    /// screen by the time this resolves.
    fn spawn_source(
        &self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let runner = source_runner(cx);
        let source = self.config.source.clone();
        let cwd = self.cwd.clone();
        let executor = cx.background_executor().clone();

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

            let result = run_source(runner, source, cwd, environment, executor).await;

            picker
                .update_in(cx, |picker, window, cx| {
                    let delegate = &mut picker.delegate;
                    match result {
                        Ok(entries) => {
                            delegate.entries = Arc::new(
                                entries
                                    .into_iter()
                                    .enumerate()
                                    .map(|(id, entry)| StringMatchCandidate::new(id, entry))
                                    .collect(),
                            );
                            delegate.state = SourceState::Ready;
                        }
                        Err(failure) => {
                            delegate.state = SourceState::Failed(failure.to_string().into());
                        }
                    }
                    let query = picker.query(cx);
                    picker.update_matches(query, window, cx);
                })
                .log_err();
        })
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

    /// Doubles as the Source's status line: the picker opens before the Source
    /// has produced anything, so an empty list must say why.
    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some(match &self.state {
            SourceState::Running => "Running…".into(),
            SourceState::Ready => "No matches".into(),
            SourceState::Failed(reason) => reason.clone(),
        })
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
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let entries = self.entries.clone();
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
                    delegate.matches = matches;
                    delegate.selected_index = 0;
                    cx.notify();
                })
                .log_err();
        })
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
        let workspace = self.workspace.clone();
        let cwd = self.cwd.clone();

        // Dismiss first so a dispatched action lands on the focus the modal was
        // covering rather than on the modal itself.
        self.dismissed(window, cx);
        cx.defer_in(window, move |_, window, cx| {
            apply_outcome(&outcome, &entry, cwd.as_ref(), &workspace, window, cx);
        });
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.finder
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
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
        match &self.state {
            SourceState::Running => Some("Running…".into()),
            SourceState::Ready => None,
            SourceState::Failed(reason) => Some(reason.clone()),
        }
    }
}
