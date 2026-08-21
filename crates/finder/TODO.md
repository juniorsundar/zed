# Finder TODO

## Completed

- [x] Field extraction for Outcome templates, including delimited Entries and
  path-with-position targets.
- [x] Query-driven Sources: `{query}` substitution, empty-query suppression,
  debounced re-runs, cancellation, and stale-result protection.

## Remaining

- [x] Positioned path previews. Finder wraps the existing preview backend so
  `open_path_at_position` previews and highlights the target line without
  modifying the upstream Picker or picker-preview crates.
- [ ] Command-driven previews.
- [x] `run_command` Outcome.
- [ ] Configurable Source working directory.
- [ ] Source-result caching.
- [ ] Directory-of-files configuration.
