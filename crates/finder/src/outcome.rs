use crate::config::{Outcome, substitute_json};
use gpui::{App, TaskExt as _, WeakEntity, Window};
use std::path::{Path, PathBuf};
use util::ResultExt as _;
use workspace::{OpenOptions, Workspace};

/// An Entry that is already absolute is used as-is; anything else is resolved
/// against the directory the Source ran in.
pub fn resolve_entry_path(entry: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(entry);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

pub fn apply_outcome(
    outcome: &Outcome,
    entry: &str,
    cwd: &Path,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    match outcome {
        Outcome::OpenPath => open_path(resolve_entry_path(entry, cwd), workspace, window, cx),
        Outcome::DispatchAction { action, args } => {
            let args = args.as_ref().map(|args| substitute_json(args, entry));
            match cx.build_action(action, args) {
                Ok(action) => window.dispatch_action(action, cx),
                Err(error) => show_error(workspace, format!("`{action}` could not run: {error}"), cx),
            }
        }
    }
}

fn open_path(
    abs_path: PathBuf,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| {
            let fs = workspace.project().read(cx).fs().clone();
            cx.spawn_in(window, async move |workspace, cx| {
                let exists = fs.metadata(&abs_path).await.ok().flatten().is_some();
                workspace.update_in(cx, |workspace, window, cx| {
                    if exists {
                        workspace
                            .open_abs_path(abs_path, OpenOptions::default(), window, cx)
                            .detach_and_log_err(cx);
                    } else {
                        workspace.show_error(
                            format!("{} no longer exists", abs_path.display()),
                            cx,
                        );
                    }
                })
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
}
