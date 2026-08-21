# Finders dispatch via a single parameterized action

GPUI collects actions at link time through the `inventory` crate
(`ActionRegistry::load_actions`), and `insert_action` is private, so no new
action *type* can be registered at runtime. Rather than patch GPUI to allow
runtime registration, Finders are reached through one statically registered
action carrying the Finder name as a parameter — `["finder::Open", { "name":
"git-branches" }]` — which the keymap loader deserializes at runtime via
`build_action(name, params)`. This is the same pattern upstream uses for
`task::Spawn { task_name }`.

## Consequences

Adding a Finder never requires a rebuild, but the keymap cannot validate Finder
names: a typo builds a valid action and fails at press time, so `finder::Open`
must report unknown and failed-to-parse Finders as user-visible errors rather
than doing nothing.

Keeping the workaround on our side of the boundary holds the upstream diff to
three lines, which is the reason this fork stays cheap to rebase.
