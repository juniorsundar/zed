# Finder TODO

## Completed

- [x] Field extraction for Outcome templates, including delimited Entries and
  path-with-position targets.
- [x] Query-driven Sources: `{query}` substitution, empty-query suppression,
  debounced re-runs, cancellation, and stale-result protection.

## Remaining

- [ ] Positioned path previews. `open_path_at_position` uses the location when
  confirming, but `picker::PreviewUpdate::from_path` accepts only a path.
  Extend Picker to carry an optional `PathWithPosition` and scroll/highlight it
  after the preview buffer loads; then pass it through Finder.
- [ ] Command-driven previews.
- [ ] `run_command` Outcome.
- [ ] Configurable Source working directory.
- [ ] Source-result caching.
- [ ] Directory-of-files configuration.
