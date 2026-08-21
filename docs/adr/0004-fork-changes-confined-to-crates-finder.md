# 4. Fork changes stay confined to fork-owned crates

## Status

Accepted

## Context

This fork tracks upstream Zed closely enough that regular upstream merges are
part of its workflow. All fork work so far — the Finder crate, its tests, and
the domain docs — lives inside `crates/finder`, so every upstream merge has
been conflict-free by construction.

The first feature to need more than `crates/finder` was positioned path
previews: showing the row/column of an `OpenPathAtPosition` Outcome in the
Picker's preview pane. The obvious implementation edits two upstream crates:
a `PreviewUpdate::from_path_with_position` constructor in `crates/picker`,
and position threading through `update_from_path` in `crates/picker_preview`.
Both files sit in an area upstream landed recently (#59604) and is actively
churning, so even small additive patches there would become a recurring,
permanent rebase tax on every sync.

## Decision

Fork changes are confined to fork-owned crates (`crates/finder`). Upstream
crates are modified only by merged upstream commits.

Where an upstream crate's public seam is insufficient, the fork composes on
top of it from the outside: implementing the public extension points it
exposes (e.g. `PreviewBackend`), wrapping existing implementations, or using
existing constructors rather than adding new variants to upstream types.
Duplicating a modest amount of logic inside a fork crate is accepted as the
price of never patching upstream code; the duplicated logic lives next to a
comment naming what it mirrors.

The sanctioned way to extend an upstream crate is to contribute the change to
Zed proper and let it arrive via merge. A local patch may bridge the gap
temporarily only if the user accepts the drift tax explicitly for that change.

## Considered Options

Minimal additive patches in the upstream crates were rejected as the default:
the runtime code is smaller and cleaner, but it makes the fork permanently
differ from upstream in files that change frequently, converting every
upstream merge into potential conflict work — the exact cost this fork's
structure exists to avoid.

Holding features until an upstream PR lands keeps the tree pristine but
couples the fork's roadmap to upstream review timelines, which is
unacceptable for exploratory work like the Finders project itself.

## Consequences

Features that would naturally extend upstream types must find an alternate
route through public seams, sometimes at the cost of indirection or small
duplication (positioned previews use a wrapping `PreviewBackend` in
`crates/finder` instead of a new `PreviewUpdate` constructor).

If such a workaround ossifies or multiplies, that is the signal to propose
the underlying change upstream: once Zed ships the general mechanism, the
fork-side workaround is deleted and the drift returns to zero.
