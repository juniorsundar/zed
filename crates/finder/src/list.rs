use crate::{open_finder, registry::FinderListEntry};
use fuzzy_nucleo::{Case, LengthPenalty, StringMatch, StringMatchCandidate};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    SharedString, Task, WeakEntity, Window,
};
use picker::{Picker, PickerDelegate};
use std::sync::Arc;
use ui::{Color, HighlightedLabel, Icon, IconName, Label, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt as _;
use workspace::{ModalView, Workspace};

pub struct FinderList {
    picker: Entity<Picker<FinderListDelegate>>,
}

impl FinderList {
    pub fn new(
        entries: Vec<FinderListEntry>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate = FinderListDelegate::new(cx.entity().downgrade(), workspace, entries);
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl Render for FinderList {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

impl Focusable for FinderList {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for FinderList {}
impl ModalView for FinderList {}

pub struct FinderListDelegate {
    finder_list: WeakEntity<FinderList>,
    workspace: WeakEntity<Workspace>,
    entries: Vec<FinderListEntry>,
    candidates: Arc<Vec<StringMatchCandidate>>,
    matches: Vec<StringMatch>,
    selected_index: usize,
}

impl FinderListDelegate {
    fn new(
        finder_list: WeakEntity<FinderList>,
        workspace: WeakEntity<Workspace>,
        entries: Vec<FinderListEntry>,
    ) -> Self {
        let candidates = entries
            .iter()
            .enumerate()
            .map(|(id, entry)| StringMatchCandidate::new(id, entry.label()))
            .collect();
        Self {
            finder_list,
            workspace,
            entries,
            candidates: Arc::new(candidates),
            matches: Vec::new(),
            selected_index: 0,
        }
    }

    fn selected_entry(&self) -> Option<&FinderListEntry> {
        let matched = self.matches.get(self.selected_index)?;
        self.entries.get(matched.candidate_id)
    }
}

impl PickerDelegate for FinderListDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "finder list"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Open a finder…".into()
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some(if self.entries.is_empty() {
            "No finders configured".into()
        } else {
            "No matches".into()
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
        let candidates = self.candidates.clone();
        cx.spawn(async move |picker, cx| {
            let matches = cx
                .background_spawn(async move {
                    if query.is_empty() {
                        candidates
                            .iter()
                            .map(|candidate| StringMatch {
                                candidate_id: candidate.id,
                                score: 0.,
                                positions: Vec::new(),
                                string: candidate.string.clone(),
                            })
                            .collect()
                    } else {
                        fuzzy_nucleo::match_strings(
                            candidates.as_slice(),
                            &query,
                            Case::Smart,
                            LengthPenalty::On,
                            100,
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
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let name = entry.name().clone();
        let workspace = self.workspace.clone();

        self.dismissed(window, cx);
        cx.defer_in(window, move |_, window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    open_finder(workspace, &name, window, cx);
                })
                .log_err();
        });
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.finder_list
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
        let matched = self.matches.get(ix)?;
        let entry = self.entries.get(matched.candidate_id)?;

        let item = ListItem::new(ix)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .child(HighlightedLabel::new(
                matched.string.clone(),
                matched.positions.clone(),
            ));

        Some(match entry {
            FinderListEntry::Finder(_) => item,
            FinderListEntry::Broken { error, .. } => item
                .start_slot(Icon::new(IconName::Warning).color(Color::Warning))
                .end_slot(Label::new(error.clone()).color(Color::Muted)),
        })
    }
}
