use std::sync::Arc;

use gpui::{AnyElement, App, AppContext as _, Context, Entity, Task, Window};
use language::Buffer;
use parking_lot::Mutex;
use picker::{MatchLocation, PreviewBackend, PreviewLayout, PreviewSource, PreviewUpdate};
use project::Project;
use rope::Point;
use util::{ResultExt as _, paths::PathWithPosition};

pub(crate) type PendingPosition = Arc<Mutex<Option<PathWithPosition>>>;

pub(crate) fn new_pending_position() -> PendingPosition {
    Arc::new(Mutex::new(None))
}

pub(crate) fn positioned_editor_preview(
    project: Entity<Project>,
    pending_position: PendingPosition,
    window: &mut Window,
    cx: &mut App,
) -> Arc<dyn PreviewBackend> {
    let inner = picker_preview::editor_preview(project.clone(), window, cx);
    Arc::new(PositionedPreviewHandle(cx.new(|_| PositionedPreview {
        inner,
        project,
        pending_position,
        pending_update: Task::ready(()),
    })))
}

struct PositionedPreviewHandle(Entity<PositionedPreview>);

impl PreviewBackend for PositionedPreviewHandle {
    fn update(&self, update: PreviewUpdate, window: &mut Window, cx: &mut App) {
        self.0
            .update(cx, |preview, cx| preview.update(update, window, cx));
    }

    fn render(&self, layout: PreviewLayout, cx: &mut App) -> AnyElement {
        self.0
            .update(cx, |preview, cx| preview.inner.render(layout, cx))
    }

    fn adjust_to_new_size(&self, window: &mut Window, cx: &mut App) {
        self.0.update(cx, |preview, cx| {
            preview.inner.adjust_to_new_size(window, cx)
        });
    }

    fn clear(&self, cx: &mut App) {
        self.0.update(cx, |preview, cx| {
            preview.pending_update = Task::ready(());
            preview.pending_position.lock().take();
            preview.inner.clear(cx);
        });
    }
}

struct PositionedPreview {
    inner: Arc<dyn PreviewBackend>,
    project: Entity<Project>,
    pending_position: PendingPosition,
    pending_update: Task<()>,
}

impl PositionedPreview {
    fn update(&mut self, update: PreviewUpdate, window: &mut Window, cx: &mut Context<Self>) {
        let position = self.pending_position.lock().take();
        self.pending_update = Task::ready(());

        let PreviewUpdate {
            source,
            match_location,
        } = update;
        let PreviewSource::Path(path) = source else {
            self.inner.update(
                PreviewUpdate {
                    source,
                    match_location,
                },
                window,
                cx,
            );
            return;
        };

        // Resolve every Finder path here rather than forwarding unpositioned
        // paths to the inner backend. That keeps all Finder loads under this
        // task's cancellation, so an older path cannot overwrite a newer one.
        // This mirrors picker_preview's path-loading flow, but stays in Finder
        // so the fork does not patch the upstream preview crates.
        let open_task = self.project.update(cx, |project, cx| {
            match project.project_path_for_absolute_path(&path, cx) {
                Some(project_path) => {
                    if let Some(buffer) = project.get_open_buffer(&project_path, cx) {
                        Task::ready(Ok(buffer))
                    } else {
                        project.open_buffer(project_path, cx)
                    }
                }
                None => project.open_local_buffer(&path, cx),
            }
        });

        self.pending_update = cx.spawn_in(window, async move |this, cx| {
            let Some(buffer) = open_task.await.log_err() else {
                return;
            };
            this.update_in(cx, |preview, window, cx| {
                let match_location = position
                    .as_ref()
                    .map(|position| match_location_for_position(buffer.read(cx), position));
                preview.inner.update(
                    PreviewUpdate {
                        source: PreviewSource::Buffer(buffer),
                        match_location,
                    },
                    window,
                    cx,
                );
                cx.notify();
            })
            .log_err();
        });
    }
}

// This mirrors the position conversion used by outcome::open_path: external
// rows and columns are 1-based, while buffer points and offsets are 0-based.
fn match_location_for_position(buffer: &Buffer, position: &PathWithPosition) -> MatchLocation {
    let snapshot = buffer.text_snapshot();
    let point = snapshot.point_from_external_input(
        position.row.unwrap_or(1).saturating_sub(1),
        position.column.unwrap_or(1).saturating_sub(1),
    );
    let start = Point::new(point.row, 0);
    let end = Point::new(point.row, snapshot.line_len(point.row));

    MatchLocation {
        anchor_range: snapshot.anchor_before(start)..snapshot.anchor_after(end),
        range: snapshot.point_to_offset(start)..snapshot.point_to_offset(end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{IntoElement, Render, TestAppContext, div};
    use std::{ops::Range, path::Path};

    struct TestView {
        _preview: Entity<PositionedPreview>,
    }

    impl Render for TestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    struct RecordingPreview {
        match_ranges: Arc<Mutex<Vec<Option<Range<usize>>>>>,
    }

    impl PreviewBackend for RecordingPreview {
        fn update(&self, update: PreviewUpdate, _window: &mut Window, _cx: &mut App) {
            self.match_ranges
                .lock()
                .push(update.match_location.map(|location| location.range));
        }

        fn render(&self, _layout: PreviewLayout, _cx: &mut App) -> AnyElement {
            div().into_any_element()
        }

        fn adjust_to_new_size(&self, _window: &mut Window, _cx: &mut App) {}

        fn clear(&self, _cx: &mut App) {}
    }

    #[gpui::test]
    async fn positioned_preview_highlights_the_requested_line(cx: &mut TestAppContext) {
        let buffer = cx.new(|cx| Buffer::local("one\ntwo\nthree\n", cx));
        let location = buffer.read_with(cx, |buffer, _| {
            match_location_for_position(
                buffer,
                &PathWithPosition {
                    path: "multi.rs".into(),
                    row: Some(2),
                    column: Some(2),
                },
            )
        });

        assert_eq!(location.range, 4..7);
    }

    #[gpui::test]
    async fn positioned_preview_loads_the_file_before_forwarding_the_highlight(
        cx: &mut TestAppContext,
    ) {
        let app_state = cx.update(|cx| workspace::AppState::test(cx));
        app_state
            .fs
            .as_fake()
            .insert_tree(
                Path::new("/project"),
                serde_json::json!({
                    "multi.rs": "one\ntwo\nthree\n",
                }),
            )
            .await;
        let project = Project::test(app_state.fs.clone(), ["/project".as_ref()], cx).await;
        let match_ranges = Arc::new(Mutex::new(Vec::new()));
        let pending_position = new_pending_position();
        *pending_position.lock() = Some(PathWithPosition {
            path: "/project/multi.rs".into(),
            row: Some(2),
            column: Some(2),
        });
        let inner = Arc::new(RecordingPreview {
            match_ranges: match_ranges.clone(),
        });
        let _window = cx.add_window(|window, cx| {
            let positioned_preview = cx.new(|_| PositionedPreview {
                inner,
                project,
                pending_position,
                pending_update: Task::ready(()),
            });
            PositionedPreviewHandle(positioned_preview.clone()).update(
                PreviewUpdate::from_path("/project/multi.rs".into()),
                window,
                cx,
            );
            TestView {
                _preview: positioned_preview,
            }
        });
        cx.run_until_parked();

        assert_eq!(*match_ranges.lock(), vec![Some(4..7)]);
    }
}
