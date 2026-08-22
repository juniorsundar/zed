use crate::config::{FinderConfig, ParsedConfig, parse_config};
use fs::Fs;
use futures::StreamExt as _;
use gpui::{App, Context, Entity, Global, SharedString, Task};
use settings::watch_config_file;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

pub const CONFIG_FILE_NAME: &str = "finders.toml";

/// What a Finder name resolves to.
///
/// A Finder that failed to parse stays addressable so that opening it can
/// report the parse failure rather than claiming it does not exist.
pub enum FinderLookup {
    Found(Arc<FinderConfig>),
    Broken { error: SharedString },
    Missing,
}

/// One row of the Finder list.
pub enum FinderListEntry {
    Finder(Arc<FinderConfig>),
    Broken {
        name: SharedString,
        error: SharedString,
    },
}

impl FinderListEntry {
    pub fn name(&self) -> &SharedString {
        match self {
            FinderListEntry::Finder(config) => &config.name,
            FinderListEntry::Broken { name, .. } => name,
        }
    }

    pub fn label(&self) -> SharedString {
        match self {
            FinderListEntry::Finder(config) => config.label.clone(),
            FinderListEntry::Broken { name, .. } => name.clone(),
        }
    }
}

pub struct FinderRegistry {
    finders: BTreeMap<SharedString, Arc<FinderConfig>>,
    errors: BTreeMap<SharedString, SharedString>,
    file_error: Option<SharedString>,
    _watch_task: Task<()>,
}

struct GlobalFinderRegistry(Entity<FinderRegistry>);

impl Global for GlobalFinderRegistry {}

impl FinderRegistry {
    pub fn set_global(registry: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalFinderRegistry(registry));
    }

    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalFinderRegistry>()
            .map(|global| global.0.clone())
    }

    pub fn new(fs: Arc<dyn Fs>, path: PathBuf, cx: &mut Context<Self>) -> Self {
        let (mut contents, watcher) = watch_config_file(cx.background_executor(), fs, path);
        let watch_task = cx.spawn(async move |registry, cx| {
            // The watcher stops delivering events once dropped.
            let _watcher = watcher;
            while let Some(contents) = contents.next().await {
                let updated = registry.update(cx, |registry, cx| {
                    registry.apply(&contents);
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
        });

        Self {
            finders: BTreeMap::default(),
            errors: BTreeMap::default(),
            file_error: None,
            _watch_task: watch_task,
        }
    }

    fn apply(&mut self, contents: &str) {
        match parse_config(contents) {
            Ok(ParsedConfig { finders, errors }) => {
                self.finders = finders;
                self.errors = errors;
                self.file_error = None;
            }
            Err(error) => {
                // The file is not valid TOML, so nothing in it can be trusted.
                self.finders.clear();
                self.errors.clear();
                self.file_error = Some(format!("{error:#}").into());
            }
        }
    }

    pub fn lookup(&self, name: &str) -> FinderLookup {
        if let Some(config) = self.finders.get(name) {
            return FinderLookup::Found(config.clone());
        }
        if let Some(error) = self.errors.get(name) {
            return FinderLookup::Broken {
                error: error.clone(),
            };
        }
        FinderLookup::Missing
    }

    /// Every Finder and every failed Finder, for the Finder list.
    pub fn entries(&self) -> Vec<FinderListEntry> {
        let finders = self.finders.values().cloned().map(FinderListEntry::Finder);
        let broken = self
            .errors
            .iter()
            .map(|(name, error)| FinderListEntry::Broken {
                name: name.clone(),
                error: error.clone(),
            });
        finders.chain(broken).collect()
    }

    /// Set when the config file as a whole could not be read as TOML.
    pub fn file_error(&self) -> Option<&SharedString> {
        self.file_error.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{AppContext as _, TestAppContext};
    use std::path::Path;

    const CONFIG_PATH: &str = "/config/finders.toml";

    const ONE_FINDER: &str = r#"
        [finder.project-files]
        source = { type = "command", command = "git", args = ["ls-files"] }
        outcome = { type = "open_path" }
    "#;

    async fn registry_over(
        contents: &str,
        cx: &mut TestAppContext,
    ) -> (Arc<FakeFs>, Entity<FinderRegistry>) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            Path::new("/config"),
            serde_json::json!({ CONFIG_FILE_NAME: contents }),
        )
        .await;

        let registry = cx.update(|cx| {
            cx.new(|cx| FinderRegistry::new(fs.clone(), PathBuf::from(CONFIG_PATH), cx))
        });
        cx.run_until_parked();
        (fs, registry)
    }

    async fn rewrite(fs: &Arc<FakeFs>, contents: &str, cx: &mut TestAppContext) {
        fs.insert_file(Path::new(CONFIG_PATH), contents.as_bytes().to_vec())
            .await;
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn loads_finders_from_the_config_file(cx: &mut TestAppContext) {
        let (_fs, registry) = registry_over(ONE_FINDER, cx).await;

        registry.read_with(cx, |registry, _| {
            assert!(matches!(
                registry.lookup("project-files"),
                FinderLookup::Found(_)
            ));
        });
    }

    #[gpui::test]
    async fn reports_an_unknown_finder_as_missing(cx: &mut TestAppContext) {
        let (_fs, registry) = registry_over(ONE_FINDER, cx).await;

        registry.read_with(cx, |registry, _| {
            assert!(matches!(
                registry.lookup("nonexistent"),
                FinderLookup::Missing
            ));
        });
    }

    #[gpui::test]
    async fn distinguishes_a_broken_finder_from_a_missing_one(cx: &mut TestAppContext) {
        let (_fs, registry) = registry_over(
            r#"
            [finder.broken]
            source = { type = "command" }
            outcome = { type = "open_path" }
            "#,
            cx,
        )
        .await;

        registry.read_with(cx, |registry, _| {
            assert!(matches!(
                registry.lookup("broken"),
                FinderLookup::Broken { .. }
            ));
            assert!(matches!(registry.lookup("absent"), FinderLookup::Missing));
        });
    }

    #[gpui::test]
    async fn a_broken_finder_leaves_its_siblings_usable(cx: &mut TestAppContext) {
        let (_fs, registry) = registry_over(
            r#"
            [finder.good]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }

            [finder.bad]
            source = { type = "command" }
            outcome = { type = "open_path" }
            "#,
            cx,
        )
        .await;

        registry.read_with(cx, |registry, _| {
            assert!(matches!(registry.lookup("good"), FinderLookup::Found(_)));
            assert!(matches!(
                registry.lookup("bad"),
                FinderLookup::Broken { .. }
            ));
            assert_eq!(registry.entries().len(), 2);
        });
    }

    #[gpui::test]
    async fn picks_up_edits_without_a_restart(cx: &mut TestAppContext) {
        let (fs, registry) = registry_over(ONE_FINDER, cx).await;

        rewrite(
            &fs,
            r#"
            [finder.renamed]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            "#,
            cx,
        )
        .await;

        registry.read_with(cx, |registry, _| {
            assert!(matches!(registry.lookup("renamed"), FinderLookup::Found(_)));
            assert!(matches!(
                registry.lookup("project-files"),
                FinderLookup::Missing
            ));
        });
    }

    #[gpui::test]
    async fn a_finder_recovers_once_its_config_is_fixed(cx: &mut TestAppContext) {
        let (fs, registry) = registry_over(
            r#"
            [finder.project-files]
            source = { type = "command" }
            outcome = { type = "open_path" }
            "#,
            cx,
        )
        .await;

        registry.read_with(cx, |registry, _| {
            assert!(matches!(
                registry.lookup("project-files"),
                FinderLookup::Broken { .. }
            ));
        });

        rewrite(&fs, ONE_FINDER, cx).await;

        registry.read_with(cx, |registry, _| {
            assert!(matches!(
                registry.lookup("project-files"),
                FinderLookup::Found(_)
            ));
        });
    }

    #[gpui::test]
    async fn a_malformed_file_is_reported_as_a_file_error(cx: &mut TestAppContext) {
        let (_fs, registry) = registry_over("this is not toml {{{", cx).await;

        registry.read_with(cx, |registry, _| {
            assert!(registry.file_error().is_some());
            assert!(registry.entries().is_empty());
        });
    }
}
