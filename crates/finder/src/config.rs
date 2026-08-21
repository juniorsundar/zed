use anyhow::{Context as _, Result};
use gpui::SharedString;
use serde::Deserialize;
use std::{collections::BTreeMap, sync::Arc};

/// Where a Finder's Entries come from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    Command {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

/// What happens when an Entry is chosen.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    OpenPath,
    DispatchAction {
        action: String,
        #[serde(default)]
        args: Option<serde_json::Value>,
    },
}

/// What the Preview pane shows for the selected Entry.
///
/// Tagged rather than a bare boolean so that a Preview driven by a command can
/// be added without breaking configs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Preview {
    /// The Entry names a file; show its contents.
    Path,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinderConfig {
    pub name: SharedString,
    pub label: SharedString,
    pub placeholder: SharedString,
    pub source: Source,
    pub outcome: Outcome,
    pub preview: Option<Preview>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinderBody {
    label: Option<SharedString>,
    placeholder: Option<SharedString>,
    source: Source,
    outcome: Outcome,
    #[serde(default)]
    preview: Option<Preview>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    finder: BTreeMap<String, toml::Value>,
}

/// The outcome of reading one config file. Finders that failed to deserialize
/// are kept as errors rather than dropped, so that opening one by name can
/// report why it is unusable instead of claiming it does not exist.
#[derive(Debug, Default)]
pub struct ParsedConfig {
    pub finders: BTreeMap<SharedString, Arc<FinderConfig>>,
    pub errors: BTreeMap<SharedString, SharedString>,
}

/// Parses a whole config file.
///
/// Returns `Err` only when the file is not valid TOML at all; a single
/// malformed Finder is recorded in [`ParsedConfig::errors`] so that its
/// siblings keep working.
pub fn parse_config(contents: &str) -> Result<ParsedConfig> {
    let file: ConfigFile = toml::from_str(contents).context("parsing finder config")?;

    let mut parsed = ParsedConfig::default();
    for (name, value) in file.finder {
        let name = SharedString::from(name);
        match value.try_into::<FinderBody>() {
            Ok(body) => {
                parsed
                    .finders
                    .insert(name.clone(), Arc::new(body.into_config(name)));
            }
            Err(error) => {
                parsed.errors.insert(name, error.to_string().into());
            }
        }
    }
    Ok(parsed)
}

impl FinderBody {
    fn into_config(self, name: SharedString) -> FinderConfig {
        let label = self.label.unwrap_or_else(|| name.clone());
        let placeholder = self
            .placeholder
            .unwrap_or_else(|| format!("Search {label}…").into());
        FinderConfig {
            name,
            label,
            placeholder,
            source: self.source,
            outcome: self.outcome,
            preview: self.preview,
        }
    }
}

/// Replaces every `{}` in `args` with `entry`.
pub fn substitute_args(args: &[String], entry: &str) -> Vec<String> {
    args.iter().map(|arg| arg.replace("{}", entry)).collect()
}

/// Replaces every `{}` in every string leaf of `value` with `entry`.
pub fn substitute_json(value: &serde_json::Value, entry: &str) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(text.replace("{}", entry)),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| substitute_json(item, entry))
                .collect(),
        ),
        serde_json::Value::Object(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), substitute_json(value, entry)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(contents: &str) -> ParsedConfig {
        parse_config(contents).expect("expected the file itself to be valid TOML")
    }

    #[test]
    fn parses_a_minimal_finder() {
        let parsed = parse_ok(
            r#"
            [finder.project-files]
            source = { type = "command", command = "git", args = ["ls-files"] }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.errors.is_empty());
        let finder = parsed.finders.get("project-files").expect("finder present");
        assert_eq!(finder.name, SharedString::from("project-files"));
        assert_eq!(
            finder.source,
            Source::Command {
                command: "git".into(),
                args: vec!["ls-files".into()],
            }
        );
        assert_eq!(finder.outcome, Outcome::OpenPath);
    }

    #[test]
    fn label_defaults_to_the_table_key_and_placeholder_follows_the_label() {
        let parsed = parse_ok(
            r#"
            [finder.git-branches]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            "#,
        );

        let finder = parsed.finders.get("git-branches").expect("finder present");
        assert_eq!(finder.label, SharedString::from("git-branches"));
        assert_eq!(finder.placeholder, SharedString::from("Search git-branches…"));
    }

    #[test]
    fn explicit_label_and_placeholder_win() {
        let parsed = parse_ok(
            r#"
            [finder.git-branches]
            label = "Git Branches"
            placeholder = "Switch branch…"
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            "#,
        );

        let finder = parsed.finders.get("git-branches").expect("finder present");
        assert_eq!(finder.label, SharedString::from("Git Branches"));
        assert_eq!(finder.placeholder, SharedString::from("Switch branch…"));
    }

    #[test]
    fn placeholder_derives_from_an_explicit_label() {
        let parsed = parse_ok(
            r#"
            [finder.git-branches]
            label = "Git Branches"
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            "#,
        );

        let finder = parsed.finders.get("git-branches").expect("finder present");
        assert_eq!(finder.placeholder, SharedString::from("Search Git Branches…"));
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        let parsed = parse_ok(
            r#"
            [finder.typo]
            lable = "Misspelled"
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.finders.is_empty());
        assert!(parsed.errors.contains_key("typo"));
    }

    #[test]
    fn a_missing_source_is_rejected() {
        let parsed = parse_ok(
            r#"
            [finder.incomplete]
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.finders.is_empty());
        assert!(parsed.errors.contains_key("incomplete"));
    }

    #[test]
    fn an_unknown_source_type_is_rejected() {
        let parsed = parse_ok(
            r#"
            [finder.futuristic]
            source = { type = "query_driven", command = "rg" }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.finders.is_empty());
        assert!(parsed.errors.contains_key("futuristic"));
    }

    #[test]
    fn one_broken_finder_leaves_its_siblings_working() {
        let parsed = parse_ok(
            r#"
            [finder.good]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }

            [finder.bad]
            source = { type = "command" }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.finders.contains_key("good"));
        assert!(parsed.errors.contains_key("bad"));
    }

    #[test]
    fn a_duplicate_finder_name_is_rejected_by_toml_itself() {
        let error = parse_config(
            r#"
            [finder.repeated]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }

            [finder.repeated]
            source = { type = "command", command = "ls" }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(error.is_err());
    }

    #[test]
    fn an_empty_file_yields_no_finders() {
        let parsed = parse_ok("");
        assert!(parsed.finders.is_empty());
        assert!(parsed.errors.is_empty());
    }

    #[test]
    fn dispatch_action_carries_its_arguments() {
        let parsed = parse_ok(
            r#"
            [finder.tasks]
            source = { type = "command", command = "git" }
            outcome = { type = "dispatch_action", action = "task::Spawn", args = { task_name = "checkout-{}" } }
            "#,
        );

        let finder = parsed.finders.get("tasks").expect("finder present");
        let Outcome::DispatchAction { action, args } = &finder.outcome else {
            panic!("expected a dispatch_action outcome");
        };
        assert_eq!(action, "task::Spawn");
        assert_eq!(
            args.as_ref().and_then(|args| args.get("task_name")),
            Some(&serde_json::Value::String("checkout-{}".into()))
        );
    }

    /// TOML forbids bare newlines inside an inline table, but permits them
    /// inside an array that the inline table contains. The shipped example
    /// config relies on this to keep long argument lists readable.
    #[test]
    fn a_multi_line_argument_array_inside_an_inline_table_parses() {
        let parsed = parse_ok(
            r#"
            [finder.git-files]
            source = { type = "command", command = "git", args = [
              "ls-files",
              "--cached",
              "--others",
            ] }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.errors.is_empty(), "errors: {:?}", parsed.errors);
        let finder = parsed.finders.get("git-files").expect("finder present");
        assert_eq!(
            finder.source,
            Source::Command {
                command: "git".into(),
                args: vec!["ls-files".into(), "--cached".into(), "--others".into()],
            }
        );
    }

    #[test]
    fn a_finder_has_no_preview_unless_it_asks_for_one() {
        let parsed = parse_ok(
            r#"
            [finder.plain]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            "#,
        );

        let finder = parsed.finders.get("plain").expect("finder present");
        assert_eq!(finder.preview, None);
    }

    #[test]
    fn a_path_preview_is_declared_explicitly() {
        let parsed = parse_ok(
            r#"
            [finder.files]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            preview = { type = "path" }
            "#,
        );

        let finder = parsed.finders.get("files").expect("finder present");
        assert_eq!(finder.preview, Some(Preview::Path));
    }

    #[test]
    fn an_unknown_preview_type_is_rejected() {
        let parsed = parse_ok(
            r#"
            [finder.futuristic]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            preview = { type = "command", command = "bat" }
            "#,
        );

        assert!(parsed.finders.is_empty());
        assert!(parsed.errors.contains_key("futuristic"));
    }

    #[test]
    fn substitution_replaces_every_placeholder_in_args() {
        assert_eq!(
            substitute_args(&["log".into(), "{}".into(), "{}..HEAD".into()], "main"),
            vec!["log".to_string(), "main".to_string(), "main..HEAD".to_string()]
        );
    }

    #[test]
    fn substitution_leaves_args_without_a_placeholder_alone() {
        assert_eq!(
            substitute_args(&["ls-files".into()], "main"),
            vec!["ls-files".to_string()]
        );
    }

    #[test]
    fn substitution_reaches_string_leaves_at_any_depth() {
        let value = serde_json::json!({
            "name": "{}",
            "nested": { "deep": ["{}", 7, true] },
        });

        assert_eq!(
            substitute_json(&value, "main"),
            serde_json::json!({
                "name": "main",
                "nested": { "deep": ["main", 7, true] },
            })
        );
    }
}
