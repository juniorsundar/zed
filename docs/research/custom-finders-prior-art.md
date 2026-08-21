# Custom finder prior art

Research date: 2026-08-21

## Local feature

The `finder` crate adds a declarative, hot-reloaded `finders.toml` registry.
Each named Finder streams newline-delimited output from a command in the first
visible worktree's resolved environment into a native fuzzy Picker. A confirmed
Entry either opens a path or dispatches a configured Zed action with `{}`
substitution. A Finder may also request a path preview.

## Zed upstream

No upstream Zed feature found combines those capabilities into a user-defined
configuration surface.

| Related upstream facility | What it provides | Difference from a Finder |
| --- | --- | --- |
| [Picker framework](https://github.com/zed-industries/zed/blob/main/crates/picker/src/picker.rs) | Native fuzzy-picker UI, delegates, matching, and preview hooks. | A Rust-level framework. New picker behavior requires a compiled delegate; it is not configurable by users. |
| [Tasks](https://zed.dev/docs/tasks) | User-defined commands and a picker to select a task. | Task output is sent to an integrated terminal; output is not streamed back as selectable picker Entries and selection cannot be configured to dispatch an arbitrary editor action. |
| [Command palette hooks](https://github.com/zed-industries/zed/blob/main/crates/command_palette_hooks/src/command_palette_hooks.rs) | Rust code can add command-palette results. | An internal code API, not a config-defined picker or command-output source. |
| [Extension development](https://zed.dev/docs/extensions/developing-extensions) | Language, debugger, theme, snippet, and related extension facilities. | No public extension UI API for defining native custom pickers was found. |

The upstream action reference has built-in finder actions such as the file
finder and command palette, but no `finder::Open` action or named custom-finder
registry: <https://zed.dev/docs/all-actions>.

## External prior art

The individual pieces are established in other editors; the distinct aspect is
the declarative, no-code composition of them.

| Project | Similarity | Important difference |
| --- | --- | --- |
| [Neovim Telescope](https://github.com/nvim-telescope/telescope.nvim/blob/master/doc/telescope.txt) | Custom finders can provide data (including command-derived data) to fuzzy pickers, define previewers, and run arbitrary Lua actions on selection. | Authors write Lua and mappings rather than declaring named hot-reloaded TOML configurations. |
| [VS Code QuickPick API](https://code.visualstudio.com/api/references/vscode-api) | Extensions can populate a QuickPick dynamically and use `commands.executeCommand` after selection. | Requires extension code; QuickPick has no native file-preview pane or declarative command-to-picker schema. |

## Conclusion

The local feature is not a new picker primitive: Zed already supplies Picker,
matching, previews, actions, project environments, and config-file watching.
Its contribution is an end-user-facing composition layer over those primitives:
a named, hot-reloaded command-to-native-picker pipeline with declarative
Outcomes. No shipped upstream Zed equivalent was found. Telescope is the
closest conceptual precedent, but its extension model is programmatic rather
than declarative.

## Research limits

This conclusion is based on official Zed source/docs and targeted upstream
issue/code searches. It is negative evidence, not a proof that no abandoned or
private proposal exists. The upstream GitHub code searches most useful for
continued verification are:

- <https://github.com/search?q=repo%3Azed-industries%2Fzed+%22finders.toml%22&type=code>
- <https://github.com/search?q=repo%3Azed-industries%2Fzed+%22finder%3A%3AOpen%22&type=code>
- <https://github.com/zed-industries/zed/issues?q=is%3Aissue+%22custom+picker%22>
