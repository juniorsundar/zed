use crate::{
    config::{FinderConfig, Preview},
    outcome::{apply_outcome, resolve_entry_path},
    source::{MAX_ENTRIES, SourceUpdate, run_source, source_runner},
};
use collections::HashMap;
use futures::{StreamExt as _, channel::mpsc};
use fuzzy_nucleo::{Case, LengthPenalty, StringMatch, StringMatchCandidate};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    HighlightStyle, IntoElement, Render, SharedString, Task, WeakEntity, Window,
};
use picker::{HighlightedTextBuilder, Picker, PickerDelegate, PreviewUpdate};
use project::Project;
use std::{path::Path, sync::Arc};
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
        let delegate = FinderDelegate::new(
            cx.entity().downgrade(),
            workspace,
            config,
            cwd,
            project.clone(),
        );
        let picker = cx.new(|cx| {
            let mut picker = if previewed {
                let preview = picker_preview::editor_preview(project.clone(), window, cx);
                Picker::uniform_list_with_preview(delegate, preview, window, cx)
            } else {
                Picker::uniform_list(delegate, window, cx)
            };
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
        // No width here: the Picker sizes itself, and grows to its larger
        // "telescope" shape when a Preview is showing. Constraining it leaves
        // the results pane with nothing to draw in.
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

/// How far the Source has got. Entries can arrive before any of these settle,
/// and can outlive a failure, so this is not a state machine over the list.
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
    /// Set when a re-match is caused by Entries arriving rather than by the
    /// user typing, so the selection does not jump out from under them.
    keep_selected_entry: bool,
    source_task: Task<()>,
}

impl FinderDelegate {
    fn new(
        finder: WeakEntity<FinderPicker>,
        workspace: WeakEntity<Workspace>,
        config: Arc<FinderConfig>,
        cwd: Arc<Path>,
        project: Entity<Project>,
    ) -> Self {
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
        }
    }

    /// Runs the Source, appending Entries as they arrive. The picker is already
    /// on screen before the first batch lands.
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

            let (sender, mut updates) = mpsc::unbounded();
            // Dropping this task drops the Source's stream, which kills the
            // child process; that happens when the picker itself is dropped.
            let _source = cx.background_spawn(run_source(
                runner,
                source,
                cwd,
                environment,
                executor,
                sender,
            ));

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

    fn apply(&mut self, update: SourceUpdate) {
        match update {
            SourceUpdate::Entries(entries) => {
                let first_id = self.entries.len();
                Arc::make_mut(&mut self.entries).extend(
                    entries.into_iter().enumerate().map(|(offset, entry)| {
                        StringMatchCandidate::new(first_id + offset, entry)
                    }),
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

    /// A message about the Source that the list itself cannot carry: a failure
    /// that arrived after usable Entries did, or a Source cut short at the cap.
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

    /// Doubles as the Source's status line: the picker opens before the Source
    /// has produced anything, so an empty list must say why.
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
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
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

    fn try_get_preview_data_for_match(&self, cx: &App) -> Option<PreviewUpdate> {
        let Some(Preview::Path) = self.config.preview else {
            return None;
        };
        let entry = self.matches.get(self.selected_index)?;
        let path = resolve_entry_path(&entry.string, &self.cwd);

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
                let mut message = HighlightedTextBuilder::default();
                message.push_styled(
                    path.display(),
                    HighlightStyle {
                        color: Some(cx.theme().colors().text_accent),
                        ..Default::default()
                    },
                );
                message.push_plain(format!(" {reason}."));
                return Some(PreviewUpdate::message(message.build()));
            }
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
                .child(Label::new(message).size(LabelSize::Small).color(Color::Muted))
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
