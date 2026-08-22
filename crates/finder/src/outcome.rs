use crate::config::{Outcome, substitute, substitute_json};
use editor::Editor;
use gpui::{App, TaskExt as _, WeakEntity, Window};
use std::path::{Path, PathBuf};
use util::{ResultExt as _, paths::PathWithPosition};
use workspace::{OpenOptions, Workspace};

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
    workspace: &WeakEntity<Workspace>,
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
