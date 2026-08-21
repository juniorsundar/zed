use crate::{
    Open,
    QUERY_DEBOUNCE,
    delegate::{FinderDelegate, FinderPicker},
    init_with_registry,
    registry::FinderRegistry,
    source::{
        FLUSH_INTERVAL, SOURCE_TIMEOUT,
        set_source_runner,
        test_support::{ScriptedRunner, Step, emitting, exit_ok, exit_with},
    },
};
use editor::Editor;
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use multi_buffer::MultiBufferOffset;
use picker::{Picker, PickerDelegate as _, PreviewSource};
use project::Project;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use workspace::{AppState, MultiWorkspace, Workspace};

const CONFIG_PATH: &str = "/config/finders.toml";

/// A Finder whose Outcome opens the Entry as a path.
const OPEN_PATH_CONFIG: &str = r#"
    [finder.demo]
    label = "Demo"
    source = { type = "command", command = "list", args = [] }
    outcome = { type = "open_path" }
"#;

/// The same Finder, with a Preview.
const PREVIEW_CONFIG: &str = r#"
    [finder.demo]
    label = "Demo"
    source = { type = "command", command = "list", args = [] }
    outcome = { type = "open_path" }
    preview = { type = "path" }
"#;

/// A query-driven Finder: the typed Query is substituted into `args` and the
/// command re-runs per Query. Its output is the result set.
const QUERY_CONFIG: &str = r#"
    [finder.demo]
    label = "Demo"
    source = { type = "query", command = "rg", args = ["--files", "{query}"] }
    outcome = { type = "open_path" }
"#;

fn init_test(cx: &mut TestAppContext) -> Arc<AppState> {
    cx.update(|cx| {
        let state = AppState::test(cx);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);
        state
    })
}

struct Harness {
    workspace: Entity<Workspace>,
}

struct HarnessWithRunner {
    harness: Harness,
    runner: Arc<ScriptedRunner>,
}

async fn setup<'a>(
    config: &'static str,
    steps: Vec<Step>,
    cx: &'a mut TestAppContext,
) -> (Harness, &'a mut VisualTestContext) {
    let (with_runner, cx) = setup_with_runner(config, ScriptedRunnerSpec::Steps(steps), cx).await;
    (with_runner.harness, cx)
}

/// Sets up a query-driven Finder whose runner answers each `spawn` with the
/// next entry in `per_spawn`, in order.
async fn setup_query<'a>(
    config: &'static str,
    per_spawn: Vec<Vec<Step>>,
    cx: &'a mut TestAppContext,
) -> (Harness, &'a mut VisualTestContext) {
    let (with_runner, cx) = setup_with_runner(config, ScriptedRunnerSpec::PerSpawn(per_spawn), cx).await;
    (with_runner.harness, cx)
}

/// Like [`setup_query`] but also returns the installed runner, so a test can
/// inspect the args each spawn received.
async fn setup_query_with_runner<'a>(
    config: &'static str,
    per_spawn: Vec<Vec<Step>>,
    cx: &'a mut TestAppContext,
) -> (HarnessWithRunner, &'a mut VisualTestContext) {
    setup_with_runner(config, ScriptedRunnerSpec::PerSpawn(per_spawn), cx).await
}

enum ScriptedRunnerSpec {
    Steps(Vec<Step>),
    PerSpawn(Vec<Vec<Step>>),
    SpawnError(&'static str),
}

async fn setup_with_runner<'a>(
    config: &'static str,
    spec: ScriptedRunnerSpec,
    cx: &'a mut TestAppContext,
) -> (HarnessWithRunner, &'a mut VisualTestContext) {
    let app_state = init_test(cx);
    let fake_fs = app_state.fs.as_fake();

    fake_fs
        .insert_tree(
            Path::new("/project"),
            json!({
                "a.rs": "",
                "b.rs": "",
                "notes.md": "",
                "multi.rs": "one\ntwo\nthree\n",
                "sub": { "c.rs": "" },
            }),
        )
        .await;
    fake_fs
        .insert_tree(Path::new("/config"), json!({ "finders.toml": config }))
        .await;

    let executor = cx.executor();
    let runner = Arc::new(match spec {
        ScriptedRunnerSpec::Steps(steps) => ScriptedRunner::new(steps, executor.clone()),
        ScriptedRunnerSpec::PerSpawn(per_spawn) => {
            ScriptedRunner::per_spawn(per_spawn, executor.clone())
        }
        ScriptedRunnerSpec::SpawnError(reason) => {
            ScriptedRunner::failing_to_spawn(reason, executor.clone())
        }
    });
    let runner_handle = runner.clone();
    cx.update(|cx| {
        set_source_runner(runner, cx);
        let registry =
            cx.new(|cx| FinderRegistry::new(app_state.fs.clone(), PathBuf::from(CONFIG_PATH), cx));
        init_with_registry(registry, cx);
    });
    cx.run_until_parked();

    let project = Project::test(app_state.fs.clone(), ["/project".as_ref()], cx).await;
    let (multi_workspace, cx) =
        cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
    let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());

    (HarnessWithRunner {
        harness: Harness { workspace },
        runner: runner_handle,
    }, cx)
}

fn open_demo(cx: &mut VisualTestContext) {
    cx.dispatch_action(Open {
        name: "demo".into(),
    });
}

fn active_finder(
    harness: &Harness,
    cx: &mut VisualTestContext,
) -> Option<Entity<Picker<FinderDelegate>>> {
    harness.workspace.read_with(cx, |workspace, cx| {
        workspace
            .active_modal::<FinderPicker>(cx)
            .map(|finder| finder.read(cx).picker().clone())
    })
}

fn entries(picker: &Entity<Picker<FinderDelegate>>, cx: &mut VisualTestContext) -> Vec<String> {
    picker.read_with(cx, |picker, _| {
        picker
            .delegate
            .matched_entries()
            .into_iter()
            .map(|entry| entry.to_string())
            .collect()
    })
}

fn status(picker: &Entity<Picker<FinderDelegate>>, cx: &mut VisualTestContext) -> Option<String> {
    picker.read_with(cx, |picker, _| {
        picker.delegate.status_text().map(|text| text.to_string())
    })
}

fn footer(picker: &Entity<Picker<FinderDelegate>>, cx: &mut VisualTestContext) -> Option<String> {
    picker.read_with(cx, |picker, _| {
        picker.delegate.footer_text().map(|text| text.to_string())
    })
}

fn selected(picker: &Entity<Picker<FinderDelegate>>, cx: &mut VisualTestContext) -> Option<String> {
    picker.read_with(cx, |picker, _| {
        let delegate = &picker.delegate;
        delegate
            .matched_entries()
            .get(delegate.selected_index())
            .map(|entry| entry.to_string())
    })
}

#[gpui::test]
async fn opens_before_the_source_has_produced_anything(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        vec![Step::Wait(FLUSH_INTERVAL), Step::Line("a.rs"), exit_ok()],
        cx,
    )
    .await;

    open_demo(cx);

    let picker = active_finder(&harness, cx).expect("the picker is on screen already");
    assert_eq!(entries(&picker, cx), Vec::<String>::new());
    assert_eq!(status(&picker, cx).as_deref(), Some("Running…"));
}

#[gpui::test]
async fn populates_entries_once_the_source_finishes(cx: &mut TestAppContext) {
    let (harness, cx) = setup(OPEN_PATH_CONFIG, emitting(&["a.rs", "b.rs"]), cx).await;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs"]);
    assert_eq!(status(&picker, cx), None);
    assert_eq!(footer(&picker, cx), None);
}

/// Entries appear while the Source is still running, rather than only at the
/// end. The picker is usable throughout.
#[gpui::test]
async fn shows_entries_while_the_source_is_still_running(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        vec![
            Step::Line("a.rs"),
            Step::Wait(SOURCE_TIMEOUT * 4),
            Step::Line("b.rs"),
            exit_ok(),
        ],
        cx,
    )
    .await;

    open_demo(cx);
    cx.executor().advance_clock(FLUSH_INTERVAL * 2);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["a.rs"]);
    assert_eq!(
        status(&picker, cx).as_deref(),
        Some("Running…"),
        "the source is still going"
    );

    cx.executor().advance_clock(SOURCE_TIMEOUT * 5);
    cx.run_until_parked();
    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs"]);
    assert_eq!(status(&picker, cx), None);
}

/// A Source that keeps producing is never abandoned for slowness, even when the
/// gaps between Entries exceed the timeout that guards its first output.
#[gpui::test]
async fn a_slow_but_productive_source_is_not_abandoned(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        vec![
            Step::Line("a.rs"),
            Step::Wait(SOURCE_TIMEOUT * 3),
            Step::Line("b.rs"),
            exit_ok(),
        ],
        cx,
    )
    .await;

    open_demo(cx);
    cx.executor().advance_clock(SOURCE_TIMEOUT * 10);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs"]);
    assert_eq!(footer(&picker, cx), None);
}

/// The selection is anchored to the Entry, not the index: Entries arriving
/// underneath must not drag the highlight back to the top of the list.
#[gpui::test]
async fn arriving_entries_do_not_move_the_selection(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        vec![
            Step::Line("a.rs"),
            Step::Line("b.rs"),
            Step::Wait(SOURCE_TIMEOUT * 4),
            Step::Line("c.rs"),
            exit_ok(),
        ],
        cx,
    )
    .await;

    open_demo(cx);
    cx.executor().advance_clock(FLUSH_INTERVAL * 2);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    cx.dispatch_action(menu::SelectNext);
    cx.run_until_parked();
    assert_eq!(selected(&picker, cx).as_deref(), Some("b.rs"));

    cx.executor().advance_clock(SOURCE_TIMEOUT * 5);
    cx.run_until_parked();

    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs", "c.rs"]);
    assert_eq!(
        selected(&picker, cx).as_deref(),
        Some("b.rs"),
        "the selection followed the entry, not the index"
    );
}

/// Typing is the one thing that *should* reset the selection.
#[gpui::test]
async fn typing_a_query_resets_the_selection(cx: &mut TestAppContext) {
    let (harness, cx) = setup(OPEN_PATH_CONFIG, emitting(&["a.rs", "b.rs"]), cx).await;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    cx.dispatch_action(menu::SelectNext);
    cx.run_until_parked();
    assert_eq!(selected(&picker, cx).as_deref(), Some("b.rs"));

    cx.simulate_input(".rs");
    cx.run_until_parked();
    assert_eq!(
        picker.read_with(cx, |picker, _| picker.delegate.selected_index()),
        0,
        "a new query starts from the top"
    );
}

#[gpui::test]
async fn filters_entries_by_the_typed_query(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        emitting(&["a.rs", "b.rs", "notes.md"]),
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    cx.simulate_input("notes");
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["notes.md"]);
}

#[gpui::test]
async fn a_failing_source_explains_itself_in_the_empty_state(cx: &mut TestAppContext) {
    let (with_runner, cx) = setup_with_runner(
        OPEN_PATH_CONFIG,
        ScriptedRunnerSpec::SpawnError("No such file or directory"),
        cx,
    )
    .await;
    let harness = with_runner.harness;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), Vec::<String>::new());
    let status = status(&picker, cx).expect("a reason is shown");
    assert!(
        status.contains("No such file or directory"),
        "expected the spawn failure, got {status:?}"
    );
}

/// A Source that fails partway keeps the Entries it already produced; the
/// failure moves to the footer rather than replacing them.
#[gpui::test]
async fn a_late_failure_keeps_the_entries_and_moves_to_the_footer(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        vec![
            Step::Line("a.rs"),
            Step::Line("b.rs"),
            exit_with("fatal: interrupted"),
        ],
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs"]);
    let footer = footer(&picker, cx).expect("the failure is still reported");
    assert!(
        footer.contains("fatal: interrupted"),
        "expected the failure in the footer, got {footer:?}"
    );
}

#[gpui::test]
async fn a_source_that_produces_nothing_times_out(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        vec![Step::Wait(SOURCE_TIMEOUT * 10), Step::Line("too late")],
        cx,
    )
    .await;

    open_demo(cx);
    cx.executor().advance_clock(SOURCE_TIMEOUT * 2);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    let status = status(&picker, cx).expect("a reason is shown");
    assert!(
        status.contains("produced nothing"),
        "expected a timeout, got {status:?}"
    );
}

#[gpui::test]
async fn an_unknown_finder_opens_nothing(cx: &mut TestAppContext) {
    let (harness, cx) = setup(OPEN_PATH_CONFIG, vec![exit_ok()], cx).await;

    cx.dispatch_action(Open {
        name: "nonexistent".into(),
    });
    cx.run_until_parked();

    assert!(active_finder(&harness, cx).is_none());
}

#[gpui::test]
async fn a_broken_finder_opens_nothing(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        r#"
        [finder.demo]
        source = { type = "command" }
        outcome = { type = "open_path" }
        "#,
        vec![exit_ok()],
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    assert!(active_finder(&harness, cx).is_none());
}

#[gpui::test]
async fn confirming_an_open_path_outcome_opens_the_file(cx: &mut TestAppContext) {
    let (harness, cx) = setup(OPEN_PATH_CONFIG, emitting(&["a.rs"]), cx).await;

    open_demo(cx);
    cx.run_until_parked();

    cx.dispatch_action(menu::Confirm);
    cx.run_until_parked();

    let opened = harness.workspace.read_with(cx, |workspace, cx| {
        workspace
            .active_item(cx)
            .and_then(|item| item.project_path(cx))
            .map(|path| path.path.to_string())
    });
    assert_eq!(opened.as_deref(), Some("a.rs"));
    assert!(active_finder(&harness, cx).is_none(), "the modal dismissed");
}

#[gpui::test]
async fn confirming_a_dispatch_action_outcome_substitutes_the_entry(cx: &mut TestAppContext) {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::default();

    let (harness, cx) = {
        let recorded = recorded.clone();
        let (harness, cx) = setup(
            r#"
            [finder.demo]
            source = { type = "command", command = "list" }
            outcome = { type = "dispatch_action", action = "finder_test::Record", args = { value = "picked-{}" } }
            "#,
            emitting(&["a.rs"]),
            cx,
        )
        .await;

        harness.workspace.update_in(cx, |workspace, _, _| {
            workspace.register_action(move |_, action: &Record, _, _| {
                if let Ok(mut recorded) = recorded.lock() {
                    recorded.push(action.value.clone());
                }
            });
        });
        (harness, cx)
    };

    open_demo(cx);
    cx.run_until_parked();

    cx.dispatch_action(menu::Confirm);
    cx.run_until_parked();

    let recorded = recorded.lock().expect("lock").clone();
    assert_eq!(recorded, vec!["picked-a.rs".to_string()]);
    assert!(active_finder(&harness, cx).is_none(), "the modal dismissed");
}

/// `git status --porcelain` shape: the path is the second Field.
#[gpui::test]
async fn an_outcome_opens_the_path_named_by_a_field(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        r#"
        [finder.demo]
        source = { type = "command", command = "list" }
        outcome = { type = "open_path", path = "{2}" }
        "#,
        emitting(&[" M a.rs"]),
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();
    cx.dispatch_action(menu::Confirm);
    cx.run_until_parked();

    let opened = harness.workspace.read_with(cx, |workspace, cx| {
        workspace
            .active_item(cx)
            .and_then(|item| item.project_path(cx))
            .map(|path| path.path.to_string())
    });
    assert_eq!(opened.as_deref(), Some("a.rs"));
}

/// A ripgrep-shaped Entry: the trailing `:row:col` moves the cursor, and the
/// matched text after it is ignored.
#[gpui::test]
async fn an_outcome_can_jump_to_a_row_and_column(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        r#"
        [finder.demo]
        delimiter = ":"
        source = { type = "command", command = "list" }
        outcome = { type = "open_path_at_position", path = "{1}:{2}:{3}" }
        "#,
        emitting(&["multi.rs:3:2:three"]),
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();
    cx.dispatch_action(menu::Confirm);
    cx.run_until_parked();

    let editor = harness
        .workspace
        .read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<Editor>())
        })
        .expect("the file opened in an editor");

    // "one\ntwo\nthree\n": row 3, column 2 is the `h` at offset 9.
    let head = editor.update_in(cx, |editor, window, cx| {
        let snapshot = editor.snapshot(window, cx);
        editor.selections.newest::<MultiBufferOffset>(&snapshot).head()
    });
    assert_eq!(head, MultiBufferOffset(9));
}

/// A Finder that names a Field its Entries do not have reports the mistake
/// rather than opening something empty.
#[gpui::test]
async fn naming_a_missing_field_reports_instead_of_opening(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        r#"
        [finder.demo]
        source = { type = "command", command = "list" }
        outcome = { type = "open_path", path = "{3}" }
        "#,
        emitting(&["a.rs"]),
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();
    cx.dispatch_action(menu::Confirm);
    cx.run_until_parked();

    let opened = harness
        .workspace
        .read_with(cx, |workspace, cx| workspace.active_item(cx).is_some());
    assert!(!opened, "nothing should have been opened");
}

fn preview_target(
    picker: &Entity<Picker<FinderDelegate>>,
    cx: &mut VisualTestContext,
) -> Option<PreviewSource> {
    picker.read_with(cx, |picker, cx| {
        picker.delegate.preview_target(cx).map(|update| update.source)
    })
}

#[gpui::test]
async fn a_finder_without_a_preview_has_nothing_to_show(cx: &mut TestAppContext) {
    let (harness, cx) = setup(OPEN_PATH_CONFIG, emitting(&["a.rs"]), cx).await;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert!(preview_target(&picker, cx).is_none());
}

#[gpui::test]
async fn a_relative_entry_previews_the_file_under_the_working_directory(cx: &mut TestAppContext) {
    let (harness, cx) = setup(PREVIEW_CONFIG, emitting(&["sub/c.rs"]), cx).await;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert!(
        matches!(
            preview_target(&picker, cx),
            Some(PreviewSource::Path(path)) if path == Path::new("/project/sub/c.rs")
        ),
        "expected the entry resolved against the project root"
    );
}

/// An Entry naming something that is not a previewable file must say so, rather
/// than leaving the previously previewed file on screen.
#[gpui::test]
async fn an_entry_that_is_not_a_file_previews_a_message(cx: &mut TestAppContext) {
    let (harness, cx) = setup(PREVIEW_CONFIG, emitting(&["sub", "gone.rs"]), cx).await;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    let Some(PreviewSource::Message(message)) = preview_target(&picker, cx) else {
        panic!("expected a message for a directory");
    };
    assert!(
        message.text.contains("is a directory"),
        "got {:?}",
        message.text
    );

    cx.dispatch_action(menu::SelectNext);
    cx.run_until_parked();
    let Some(PreviewSource::Message(message)) = preview_target(&picker, cx) else {
        panic!("expected a message for a missing file");
    };
    assert!(
        message.text.contains("no longer exists"),
        "got {:?}",
        message.text
    );
}

/// A query-driven Finder shows nothing while the Query is empty: the Source
/// is suppressed, not run with an empty pattern.
#[gpui::test]
async fn a_query_driven_finder_runs_nothing_until_the_user_types(cx: &mut TestAppContext) {
    let (harness, cx) = setup_query(
        QUERY_CONFIG,
        vec![emitting(&["a.rs", "b.rs"])],
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), Vec::<String>::new(), "nothing ran yet");
    assert_eq!(status(&picker, cx), None, "not running, not failed");
}

/// Typing a Query spawns the Source with the Query substituted in, and the
/// Source's output is the result set — no client fuzzy re-match.
#[gpui::test]
async fn a_query_driven_source_owns_filtering(cx: &mut TestAppContext) {
    let (harness, cx) = setup_query(
        QUERY_CONFIG,
        vec![emitting(&["a.rs", "b.rs"])],
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    cx.simulate_input("foo");
    // Advance past the debounce and let the run settle.
    cx.executor().advance_clock(QUERY_DEBOUNCE + FLUSH_INTERVAL * 2);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs"]);
}

/// A new Query clears the list first; it never shows answers to the previous
/// Query while the new run is in flight.
#[gpui::test]
async fn a_new_query_clears_the_list_before_the_new_run_lands(cx: &mut TestAppContext) {
    let (harness, cx) = setup_query(
        QUERY_CONFIG,
        vec![
            emitting(&["a.rs", "b.rs"]),
            emitting(&["c.rs"]),
        ],
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    cx.simulate_input("foo");
    cx.executor().advance_clock(QUERY_DEBOUNCE + FLUSH_INTERVAL * 2);
    cx.run_until_parked();
    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs"]);

    // A new Query clears the list synchronously, before the debounce elapses
    // and before the new run has produced anything.
    cx.simulate_input("bar");
    assert_eq!(
        entries(&picker, cx),
        Vec::<String>::new(),
        "the list cleared as soon as the new query arrived"
    );

    // Once the debounce elapses and the new run lands, its entries appear.
    cx.executor().advance_clock(QUERY_DEBOUNCE + FLUSH_INTERVAL * 2);
    cx.run_until_parked();
    assert_eq!(entries(&picker, cx), vec!["c.rs"]);
}

/// Emptying the Query suppresses the Source and clears the list.
#[gpui::test]
async fn emptying_the_query_suppresses_the_source(cx: &mut TestAppContext) {
    let (harness, cx) = setup_query(
        QUERY_CONFIG,
        vec![emitting(&["a.rs", "b.rs"])],
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    cx.simulate_input("foo");
    cx.executor().advance_clock(QUERY_DEBOUNCE + FLUSH_INTERVAL * 2);
    cx.run_until_parked();
    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs"]);

    // Clear the query. No spawn should run; the list empties.
    picker.update_in(cx, |picker, window, cx| {
        picker.set_query("", window, cx);
    });
    cx.executor().advance_clock(QUERY_DEBOUNCE + FLUSH_INTERVAL * 2);
    cx.run_until_parked();
    assert_eq!(entries(&picker, cx), Vec::<String>::new());
    assert_eq!(status(&picker, cx), None);
}

/// A straggler batch from a killed run does not pollute the new run's list.
#[gpui::test]
async fn a_straggler_from_a_killed_run_is_ignored(cx: &mut TestAppContext) {
    // First run emits one line, then waits a long time before exiting — so it
    // is still in flight when the second Query arrives and kills it. Its late
    // exit must not reach the second run's list.
    let (harness, cx) = setup_query(
        QUERY_CONFIG,
        vec![
            vec![
                Step::Line("stale.rs"),
                Step::Wait(SOURCE_TIMEOUT * 8),
                exit_ok(),
            ],
            emitting(&["fresh.rs"]),
        ],
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    cx.simulate_input("foo");
    cx.executor().advance_clock(QUERY_DEBOUNCE + FLUSH_INTERVAL * 2);
    cx.run_until_parked();
    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["stale.rs"]);

    cx.simulate_input("bar");
    cx.executor().advance_clock(QUERY_DEBOUNCE + FLUSH_INTERVAL * 2);
    cx.run_until_parked();
    assert_eq!(entries(&picker, cx), vec!["fresh.rs"]);

    // Advance far enough that the killed first run's late exit would have
    // landed; it must not have added anything.
    cx.executor().advance_clock(SOURCE_TIMEOUT * 10);
    cx.run_until_parked();
    assert_eq!(entries(&picker, cx), vec!["fresh.rs"]);
}

/// Enter while a query's debounce is still pending makes the run spawn
/// immediately (bypassing the debounce) and confirms once the result lands.
#[gpui::test]
async fn an_enter_during_the_debounce_forces_the_spawn_and_confirms(cx: &mut TestAppContext) {
    let (harness, cx) = setup_query(QUERY_CONFIG, vec![emitting(&["a.rs"])], cx).await;

    open_demo(cx);
    cx.run_until_parked();

    // Type a query and confirm immediately, before the debounce elapses.
    cx.simulate_input("foo");
    cx.dispatch_action(menu::Confirm);
    // Do not advance the debounce here: the run was forced to spawn, then the
    // deferred confirm fires when its entries land.
    cx.run_until_parked();

    let opened = harness.workspace.read_with(cx, |workspace, cx| {
        workspace
            .active_item(cx)
            .and_then(|item| item.project_path(cx))
            .map(|path| path.path.to_string())
    });
    assert_eq!(opened.as_deref(), Some("a.rs"));
    assert!(active_finder(&harness, cx).is_none(), "the modal dismissed");
}

/// The typed Query is substituted for `{query}` in the Source's args before
/// the process spawns.
#[gpui::test]
async fn the_query_is_substituted_into_the_spawned_args(cx: &mut TestAppContext) {
    let (with_runner, cx) = setup_query_with_runner(QUERY_CONFIG, vec![emitting(&["a.rs"])], cx).await;

    open_demo(cx);
    cx.run_until_parked();

    cx.simulate_input("foo");
    cx.executor().advance_clock(QUERY_DEBOUNCE + FLUSH_INTERVAL * 2);
    cx.run_until_parked();

    let spawned = with_runner.runner.spawned_args();
    assert_eq!(
        spawned,
        vec![vec!["--files".to_string(), "foo".to_string()]],
        "the query replaced the {{query}} placeholder in args"
    );
}

/// Only exists so a Finder Outcome has a registered action to dispatch.
#[derive(Debug, PartialEq, Clone, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = finder_test)]
#[serde(deny_unknown_fields)]
struct Record {
    value: String,
}
