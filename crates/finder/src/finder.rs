mod config;
#[cfg(test)]
mod finder_tests;
mod delegate;
mod list;
mod outcome;
mod registry;
mod source;

use fs::Fs;
use gpui::{Action, App, AppContext as _, Context, Window, actions};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use workspace::Workspace;

pub use config::{
    FinderConfig, Outcome, ParsedConfig, Preview, Source, parse_config, substitute_args,
    substitute_json,
};
pub use delegate::{FinderDelegate, FinderPicker};
pub use list::FinderList;
pub use registry::{CONFIG_FILE_NAME, FinderListEntry, FinderLookup, FinderRegistry};
pub use source::{
    DefaultSourceRunner, FLUSH_INTERVAL, MAX_ENTRIES, SOURCE_TIMEOUT, SourceEvent, SourceFailure,
    SourceRunner, SourceStream, SourceUpdate, run_source, set_source_runner,
};

/// Opens a configured Finder by name.
///
/// Finder names are action *arguments* rather than action names because GPUI
/// collects actions at link time; see
/// `docs/adr/0001-finders-dispatch-via-a-parameterized-action.md`.
#[derive(Debug, PartialEq, Clone, Deserialize, JsonSchema, Action)]
#[action(namespace = finder)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub name: String,
}

actions!(
    finder,
    [
        /// Lists every configured Finder and opens the selected one.
        OpenFinderList
    ]
);

pub fn init(fs: Arc<dyn Fs>, cx: &mut App) {
    let path = paths::config_dir().join(CONFIG_FILE_NAME);
    let registry = cx.new(|cx| FinderRegistry::new(fs, path, cx));
    init_with_registry(registry, cx);
}

/// Registration split out so tests can supply a registry over a fake
/// filesystem instead of the real config directory.
pub fn init_with_registry(registry: gpui::Entity<FinderRegistry>, cx: &mut App) {
    FinderRegistry::set_global(registry, cx);
    cx.observe_new(register_workspace_actions).detach();
}

fn register_workspace_actions(
    workspace: &mut Workspace,
    _window: Option<&mut Window>,
    _cx: &mut Context<Workspace>,
) {
    workspace
        .register_action(|workspace, action: &Open, window, cx| {
            open_finder(workspace, &action.name, window, cx);
        })
        .register_action(|workspace, _: &OpenFinderList, window, cx| {
            open_finder_list(workspace, window, cx);
        });
}

pub fn open_finder(
    workspace: &mut Workspace,
    name: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(registry) = FinderRegistry::global(cx) else {
        return;
    };

    let config = match registry.read(cx).lookup(name) {
        FinderLookup::Found(config) => config,
        FinderLookup::Broken { error } => {
            workspace.show_error(format!("Finder `{name}` failed to load: {error}"), cx);
            return;
        }
        FinderLookup::Missing => {
            workspace.show_error(format!("No finder named `{name}`"), cx);
            return;
        }
    };

    let project = workspace.project().clone();
    let cwd = project
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|worktree| worktree.read(cx).abs_path());
    let Some(cwd) = cwd else {
        workspace.show_error(
            format!("Finder `{name}` needs an open project to run in"),
            cx,
        );
        return;
    };

    let weak_workspace = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        FinderPicker::new(config, cwd, project, weak_workspace, window, cx)
    });
}

pub fn open_finder_list(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(registry) = FinderRegistry::global(cx) else {
        return;
    };

    if let Some(error) = registry.read(cx).file_error().cloned() {
        workspace.show_error(format!("{CONFIG_FILE_NAME} could not be read: {error}"), cx);
        return;
    }

    let weak_workspace = cx.entity().downgrade();
    let entries = registry.read(cx).entries();
    workspace.toggle_modal(window, cx, move |window, cx| {
        FinderList::new(entries, weak_workspace, window, cx)
    });
}
