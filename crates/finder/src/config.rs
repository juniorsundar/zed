use anyhow::{Context as _, Result};
use gpui::SharedString;
use serde::Deserialize;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    /// Runs once at open; the Query filters its output client-side.
    Command {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Re-runs per Query with [`QUERY_PLACEHOLDER`] substituted into `args`.
    /// Rejected at parse time if no arg carries the placeholder.
    Query {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

impl Source {
    pub fn command(&self) -> &str {
        match self {
            Source::Command { command, .. } | Source::Query { command, .. } => command,
        }
    }

    pub fn args(&self) -> &[String] {
        match self {
            Source::Command { args, .. } | Source::Query { args, .. } => args,
        }
    }

    pub fn is_query_driven(&self) -> bool {
        matches!(self, Source::Query { .. })
    }
}

pub const QUERY_PLACEHOLDER: &str = "{query}";

/// Replaces every [`QUERY_PLACEHOLDER`] in `args` with `query`.
pub fn substitute_query(args: &[String], query: &str) -> Vec<String> {
    args.iter()
        .map(|arg| arg.replace(QUERY_PLACEHOLDER, query))
        .collect()
}

pub fn has_query_placeholder(args: &[String]) -> bool {
    args.iter().any(|arg| arg.contains(QUERY_PLACEHOLDER))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    OpenPath {
        /// Defaults to the whole Entry.
        #[serde(default)]
        path: Option<String>,
    },
    /// Like [`Outcome::OpenPath`], but the path carries a trailing `:row:col`
    /// and the editor jumps there.
    OpenPathAtPosition {
        #[serde(default)]
        path: Option<String>,
    },
    DispatchAction {
        action: String,
        #[serde(default)]
        args: Option<serde_json::Value>,
    },
    RunCommand {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

impl Outcome {
    /// The template naming the path, so the Preview shows what Confirm opens.
    pub fn path_template(&self) -> Option<&str> {
        match self {
            Outcome::OpenPath { path } | Outcome::OpenPathAtPosition { path } => {
                Some(path.as_deref().unwrap_or(WHOLE_ENTRY))
            }
            Outcome::DispatchAction { .. } | Outcome::RunCommand { .. } => None,
        }
    }

    pub fn path_carries_position(&self) -> bool {
        matches!(self, Outcome::OpenPathAtPosition { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Preview {
    Path,
}

/// Where a Finder's command-spawning work (its Source and any `run_command`
/// Outcome) executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RunOn {
    #[default]
    Auto,
    Local,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinderConfig {
    pub name: SharedString,
    pub label: SharedString,
    pub placeholder: SharedString,
    pub source: Source,
    pub outcome: Outcome,
    pub preview: Option<Preview>,
    pub delimiter: Option<String>,
    pub run_on: RunOn,
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
    #[serde(default)]
    delimiter: Option<String>,
    #[serde(default)]
    run_on: RunOn,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    finder: BTreeMap<String, toml::Value>,
}

/// Failed finders stay addressable so opening them can report why.
#[derive(Debug, Default)]
pub struct ParsedConfig {
    pub finders: BTreeMap<SharedString, Arc<FinderConfig>>,
    pub errors: BTreeMap<SharedString, SharedString>,
}

/// Returns `Err` only when the file is not valid TOML at all; individual
/// malformed finders land in [`ParsedConfig::errors`].
pub fn parse_config(contents: &str) -> Result<ParsedConfig> {
    let file: ConfigFile = toml::from_str(contents).context("parsing finder config")?;

    let mut parsed = ParsedConfig::default();
    for (name, value) in file.finder {
        let name = SharedString::from(name);
        match value.try_into::<FinderBody>() {
            Ok(body) => match body.try_into_config(name.clone()) {
                Ok(config) => {
                    parsed.finders.insert(name, Arc::new(config));
                }
                Err(error) => {
                    parsed.errors.insert(name, error.to_string().into());
                }
            },
            Err(error) => {
                parsed.errors.insert(name, error.to_string().into());
            }
        }
    }
    Ok(parsed)
}

impl FinderBody {
    fn try_into_config(self, name: SharedString) -> Result<FinderConfig> {
        if matches!(self.source, Source::Query { .. }) && !has_query_placeholder(self.source.args())
        {
            anyhow::bail!(
                "a `query` source must put `{{query}}` in its args, or the typed query would be ignored"
            );
        }
        Ok(self.into_config(name))
    }

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
            delimiter: self.delimiter,
            run_on: self.run_on,
        }
    }
}

pub const WHOLE_ENTRY: &str = "{}";

/// A template referenced a Field the Entry does not have. Reported rather
/// than substituted empty so the mistake is visible.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingField {
    pub index: usize,
    pub entry: String,
    pub available: usize,
}

impl std::fmt::Display for MissingField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "`{{{}}}` needs field {} but `{}` has {}",
            self.index, self.index, self.entry, self.available
        )
    }
}

/// With no delimiter, runs of whitespace separate Fields (column output
/// like `docker ps`).
pub fn fields<'a>(entry: &'a str, delimiter: Option<&str>) -> Vec<&'a str> {
    match delimiter {
        Some(delimiter) if !delimiter.is_empty() => entry.split(delimiter).collect(),
        _ => entry.split_whitespace().collect(),
    }
}

/// Replaces `{}` with the whole Entry and `{n}` with its nth Field, counting
/// from 1. Anything else in braces is left alone.
pub fn substitute(
    template: &str,
    entry: &str,
    delimiter: Option<&str>,
) -> Result<String, MissingField> {
    if !template.contains('{') {
        return Ok(template.to_owned());
    }

    let mut split = None;
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            result.push_str(&rest[start..]);
            return Ok(result);
        };

        let placeholder = &after[..end];
        if placeholder.is_empty() {
            result.push_str(entry);
        } else if let Ok(index) = placeholder.parse::<usize>()
            && index > 0
        {
            let split = split.get_or_insert_with(|| fields(entry, delimiter));
            let field = split.get(index - 1).ok_or_else(|| MissingField {
                index,
                entry: entry.to_owned(),
                available: split.len(),
            })?;
            result.push_str(field);
        } else {
            result.push('{');
            result.push_str(placeholder);
            result.push('}');
        }

        rest = &after[end + 1..];
    }

    result.push_str(rest);
    Ok(result)
}

pub fn substitute_args(
    args: &[String],
    entry: &str,
    delimiter: Option<&str>,
) -> Result<Vec<String>, MissingField> {
    args.iter()
        .map(|arg| substitute(arg, entry, delimiter))
        .collect()
}

pub fn substitute_json(
    value: &serde_json::Value,
    entry: &str,
    delimiter: Option<&str>,
) -> Result<serde_json::Value, MissingField> {
    Ok(match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(substitute(text, entry, delimiter)?)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| substitute_json(item, entry, delimiter))
                .collect::<Result<_, _>>()?,
        ),
        serde_json::Value::Object(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, value)| Ok((key.clone(), substitute_json(value, entry, delimiter)?)))
                .collect::<Result<_, MissingField>>()?,
        ),
        other => other.clone(),
    })
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
        assert_eq!(finder.outcome, Outcome::OpenPath { path: None });
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
        assert_eq!(
            finder.placeholder,
            SharedString::from("Search git-branches…")
        );
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
        assert_eq!(
            finder.placeholder,
            SharedString::from("Search Git Branches…")
        );
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
            source = { type = "futuristic", command = "rg" }
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
    fn run_command_carries_its_executable_and_arguments() {
        let parsed = parse_ok(
            r#"
            [finder.checkout]
            source = { type = "command", command = "git", args = ["branch", "--format=%(refname:short)"] }
            outcome = { type = "run_command", command = "git", args = ["checkout", "{1}"] }
            "#,
        );

        let finder = parsed.finders.get("checkout").expect("finder present");
        assert_eq!(
            finder.outcome,
            Outcome::RunCommand {
                command: "git".into(),
                args: vec!["checkout".into(), "{1}".into()],
            }
        );
    }

    #[test]
    fn run_command_defaults_to_no_arguments() {
        let parsed = parse_ok(
            r#"
            [finder.refresh]
            source = { type = "command", command = "git" }
            outcome = { type = "run_command", command = "refresh-index" }
            "#,
        );

        let finder = parsed.finders.get("refresh").expect("finder present");
        assert_eq!(
            finder.outcome,
            Outcome::RunCommand {
                command: "refresh-index".into(),
                args: Vec::new(),
            }
        );
    }

    #[test]
    fn run_command_rejects_execution_policy_fields() {
        let parsed = parse_ok(
            r#"
            [finder.checkout]
            source = { type = "command", command = "git" }
            outcome = { type = "run_command", command = "git", timeout = 5 }
            "#,
        );

        assert!(parsed.finders.is_empty());
        let error = parsed.errors.get("checkout").expect("finder error present");
        assert!(error.contains("timeout"), "unexpected error: {error}");
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
    fn a_delimiter_and_a_path_template_round_trip() {
        let parsed = parse_ok(
            r#"
            [finder.grep]
            delimiter = ":"
            source = { type = "command", command = "rg" }
            outcome = { type = "open_path_at_position", path = "{1}:{2}:{3}" }
            "#,
        );

        let finder = parsed.finders.get("grep").expect("finder present");
        assert_eq!(finder.delimiter.as_deref(), Some(":"));
        assert_eq!(
            finder.outcome,
            Outcome::OpenPathAtPosition {
                path: Some("{1}:{2}:{3}".into()),
            }
        );
        assert!(finder.outcome.path_carries_position());
    }

    #[test]
    fn an_outcome_without_a_path_template_defaults_to_the_whole_entry() {
        assert_eq!(
            Outcome::OpenPath { path: None }.path_template(),
            Some(WHOLE_ENTRY)
        );
        assert_eq!(
            Outcome::DispatchAction {
                action: "zed::OpenBrowser".into(),
                args: None,
            }
            .path_template(),
            None
        );
    }

    #[test]
    fn fields_default_to_splitting_on_whitespace_runs() {
        assert_eq!(fields("  M   src/main.rs ", None), vec!["M", "src/main.rs"]);
    }

    #[test]
    fn an_explicit_delimiter_keeps_empty_fields() {
        assert_eq!(fields("a::b", Some(":")), vec!["a", "", "b"]);
    }

    #[test]
    fn a_numbered_placeholder_names_a_field_from_one() {
        assert_eq!(
            substitute("{2}", "abc1234 Fix the crash", None),
            Ok("Fix".to_string())
        );
    }

    #[test]
    fn the_bare_placeholder_still_means_the_whole_entry() {
        assert_eq!(
            substitute("{}", "abc1234 Fix the crash", None),
            Ok("abc1234 Fix the crash".to_string())
        );
    }

    #[test]
    fn placeholders_mix_with_surrounding_text() {
        assert_eq!(
            substitute("{1}:{2}", "src/a.rs:12:3:text", Some(":")),
            Ok("src/a.rs:12".to_string())
        );
    }

    #[test]
    fn naming_a_field_that_is_not_there_is_an_error() {
        assert_eq!(
            substitute("{4}", "one two", None),
            Err(MissingField {
                index: 4,
                entry: "one two".into(),
                available: 2,
            })
        );
    }

    #[test]
    fn unrecognised_braces_are_left_alone() {
        assert_eq!(
            substitute("stash@{0} {1}", "stash@{0}: WIP", Some(": ")),
            Ok("stash@{0} stash@{0}".to_string())
        );
    }

    #[test]
    fn an_unclosed_brace_is_left_alone() {
        assert_eq!(substitute("{1", "one two", None), Ok("{1".to_string()));
    }

    #[test]
    fn a_missing_field_in_action_arguments_is_reported() {
        let value = serde_json::json!({ "name": "{9}" });
        assert!(substitute_json(&value, "one two", None).is_err());
    }

    #[test]
    fn substitution_replaces_every_placeholder_in_args() {
        assert_eq!(
            substitute_args(
                &["log".into(), "{}".into(), "{}..HEAD".into()],
                "main",
                None
            )
            .expect("no fields named"),
            vec![
                "log".to_string(),
                "main".to_string(),
                "main..HEAD".to_string()
            ]
        );
    }

    #[test]
    fn substitution_leaves_args_without_a_placeholder_alone() {
        assert_eq!(
            substitute_args(&["ls-files".into()], "main", None).expect("no fields named"),
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
            substitute_json(&value, "main", None).expect("no fields named"),
            serde_json::json!({
                "name": "main",
                "nested": { "deep": ["main", 7, true] },
            })
        );
    }

    #[test]
    fn parses_a_query_driven_finder() {
        let parsed = parse_ok(
            r#"
            [finder.demo]
            source = { type = "query", command = "rg", args = ["--line-number", "{query}"] }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.errors.is_empty());
        let finder = parsed.finders.get("demo").expect("finder present");
        assert_eq!(
            finder.source,
            Source::Query {
                command: "rg".into(),
                args: vec!["--line-number".into(), "{query}".into()],
            }
        );
        assert!(finder.source.is_query_driven());
    }

    #[test]
    fn a_query_source_without_the_placeholder_is_rejected() {
        let parsed = parse_ok(
            r#"
            [finder.demo]
            source = { type = "query", command = "rg", args = ["--files"] }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.finders.is_empty());
        let error = parsed.errors.get("demo").expect("the finder is broken");
        assert!(
            error.contains("{query}"),
            "expected the error to name the placeholder, got {error}"
        );
    }

    #[test]
    fn substitute_query_replaces_every_placeholder() {
        assert_eq!(
            substitute_query(
                &["rg".into(), "{query}".into(), "--glob=!{query}".into()],
                "foo",
            ),
            vec!["rg", "foo", "--glob=!foo"],
        );
    }

    #[test]
    fn substitute_query_leaves_args_without_the_placeholder_alone() {
        assert_eq!(
            substitute_query(&["rg".into(), "--files".into()], "foo"),
            vec!["rg", "--files"],
        );
    }

    #[test]
    fn run_on_defaults_to_auto_when_omitted() {
        let parsed = parse_ok(
            r#"
            [finder.plain]
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            "#,
        );

        let finder = parsed.finders.get("plain").expect("finder present");
        assert_eq!(finder.run_on, RunOn::Auto);
    }

    #[test]
    fn run_on_local_parses() {
        let parsed = parse_ok(
            r#"
            [finder.local-only]
            run_on = "local"
            source = { type = "command", command = "rg" }
            outcome = { type = "open_path" }
            "#,
        );

        let finder = parsed.finders.get("local-only").expect("finder present");
        assert_eq!(finder.run_on, RunOn::Local);
    }

    #[test]
    fn an_unknown_run_on_value_is_rejected() {
        let parsed = parse_ok(
            r#"
            [finder.futuristic]
            run_on = "mars"
            source = { type = "command", command = "git" }
            outcome = { type = "open_path" }
            "#,
        );

        assert!(parsed.finders.is_empty());
        let error = parsed.errors.get("futuristic").expect("finder error present");
        assert!(error.contains("run_on"), "unexpected error: {error}");
    }
}
