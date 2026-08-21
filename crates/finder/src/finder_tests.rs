use crate::{
    Open, delegate::FinderDelegate, delegate::FinderPicker, init_with_registry,
    registry::FinderRegistry, source::SourceRunner, source::set_source_runner,
};
use async_trait::async_trait;
use collections::HashMap;
use gpui::{AppContext as _, BackgroundExecutor, Entity, TestAppContext, VisualTestContext};
use picker::Picker;
use project::Project;
use serde_json::json;
use std::{
    os::unix::process::ExitStatusExt as _,
    path::{Path, PathBuf},
    process::Output,
    sync::{Arc, Mutex},
    time::Duration,
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

struct ScriptedRunner {
    output: Result<Output, String>,
    delay: Option<Duration>,
    executor: BackgroundExecutor,
}

#[async_trait]
impl SourceRunner for ScriptedRunner {
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

async fn setup<'a>(
    config: &'static str,
    output: Result<Output, String>,
    delay: Option<Duration>,
    cx: &'a mut TestAppContext,
) -> (Harness, &'a mut VisualTestContext) {
    let app_state = init_test(cx);
    let fake_fs = app_state.fs.as_fake();

    fake_fs
        .insert_tree(
            Path::new("/project"),
            json!({ "a.rs": "", "b.rs": "", "notes.md": "" }),
        )
        .await;
    fake_fs
        .insert_tree(Path::new("/config"), json!({ "finders.toml": config }))
        .await;

    let executor = cx.executor();
    cx.update(|cx| {
        set_source_runner(
            Arc::new(ScriptedRunner {
                output,
                delay,
                executor: executor.clone(),
            }),
            cx,
        );
        let registry = cx.new(|cx| {
            FinderRegistry::new(app_state.fs.clone(), PathBuf::from(CONFIG_PATH), cx)
        });
        init_with_registry(registry, cx);
    });
    cx.run_until_parked();

    let project = Project::test(app_state.fs.clone(), ["/project".as_ref()], cx).await;
    let (multi_workspace, cx) =
        cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
    let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());

    (Harness { workspace }, cx)
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

#[gpui::test]
async fn opens_before_the_source_has_produced_anything(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        succeeding("a.rs\nb.rs\n"),
        Some(Duration::from_secs(1)),
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
    let (harness, cx) = setup(OPEN_PATH_CONFIG, succeeding("a.rs\nb.rs\n"), None, cx).await;

    open_demo(cx);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    assert_eq!(entries(&picker, cx), vec!["a.rs", "b.rs"]);
    assert_eq!(status(&picker, cx), None);
}

#[gpui::test]
async fn filters_entries_by_the_typed_query(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        succeeding("a.rs\nb.rs\nnotes.md\n"),
        None,
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
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        Err("No such file or directory".into()),
        None,
        cx,
    )
    .await;

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

#[gpui::test]
async fn a_source_that_hangs_times_out(cx: &mut TestAppContext) {
    let (harness, cx) = setup(
        OPEN_PATH_CONFIG,
        succeeding("never delivered"),
        Some(crate::SOURCE_TIMEOUT * 10),
        cx,
    )
    .await;

    open_demo(cx);
    cx.executor().advance_clock(crate::SOURCE_TIMEOUT * 2);
    cx.run_until_parked();

    let picker = active_finder(&harness, cx).expect("finder open");
    let status = status(&picker, cx).expect("a reason is shown");
    assert!(
        status.contains("did not finish"),
        "expected a timeout, got {status:?}"
    );
}

#[gpui::test]
async fn an_unknown_finder_opens_nothing(cx: &mut TestAppContext) {
    let (harness, cx) = setup(OPEN_PATH_CONFIG, succeeding(""), None, cx).await;

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
        succeeding(""),
        None,
        cx,
    )
    .await;

    open_demo(cx);
    cx.run_until_parked();

    assert!(active_finder(&harness, cx).is_none());
}

#[gpui::test]
async fn confirming_an_open_path_outcome_opens_the_file(cx: &mut TestAppContext) {
    let (harness, cx) = setup(OPEN_PATH_CONFIG, succeeding("a.rs\n"), None, cx).await;

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
            succeeding("a.rs\n"),
            None,
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

/// Only exists so a Finder Outcome has a registered action to dispatch.
#[derive(Debug, PartialEq, Clone, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = finder_test)]
#[serde(deny_unknown_fields)]
struct Record {
    value: String,
}
