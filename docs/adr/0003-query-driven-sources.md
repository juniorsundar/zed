# 3. A query-driven Source owns filtering; the Picker does not re-match

## Status

Accepted

## Context

A Finder's Source originally ran once at open and produced every Entry; the
Picker then fuzzy-matched whatever the user typed against that set with
`fuzzy_nucleo`. The Query was purely a client-side filter with no domain
status — `CONTEXT.md` had no entry for it.

To support backends like `rg` or `git grep`, where running the command over
the whole tree and then filtering client-side is impractical, the Query has to
reach the Source itself. That makes the Query a Source input, and raises the
question of what the client's role becomes.

## Decision

A query-driven Source is a new `Source::Query { command, args }` variant. It
re-spawns the command per Query: each Query change (debounced) kills the
in-flight run and spawns a fresh process, with the typed Query substituted into
`args` via a `{query}` placeholder. The Source's output *is* the result set;
the Picker does not fuzzy-match it again. For a one-shot `Source::Command`, the
Query stays a client-side fuzzy filter over the Entries already produced.

So the Query has one role per Source variant, which `CONTEXT.md` now records:
client filter for `Command`, Source input for `Query`.

A `Source::Query` whose `args` contain no `{query}` is rejected at parse time
as a broken Finder, since the typed Query would otherwise be silently ignored.
The empty Query suppresses the spawn rather than running the command with an
empty pattern, which for most backends matches everything or errors. On each
new spawn the list clears and shows "Running…", so the list is always the
answer to the current Query or empty — never a stale answer to the previous
one.

## Considered Options

A long-lived interactive process fed queries over stdin was rejected: it
requires inventing a query/response framing protocol, is harder to cancel
cleanly, and almost no real backend (`rg`, `fd`, `git grep`) speaks one.
Re-spawn per query keeps cancellation equivalent to dropping the stream, which
the existing `run_source` and `SourceRunner` seam already model.

Letting the Source pre-filter while the Picker still fuzzy-matched (fzf's
model) was rejected: it gives the Query two roles for one Finder and interprets
the typed text with two different matchers, which is surprising when they
disagree. It would also prevent `CONTEXT.md` from stating that the Query's role
is determined by the Source variant.

Extending `Source::Command` with a `query_driven` bool (or an implicit trigger
from a `{query}` in `args`) was rejected: the two behaviours have genuinely
different lifecycles, cancellation, and empty-Query semantics, and flattening
them into one variant hides that behind a flag or magic.

## Consequences

A query-driven Finder shows nothing while the Query is empty, and clears its
list on every Query change. The brief empty state between spawn and first batch
is the cost of the Source owning filtering; it is bounded by the debounce and
the existing running indicator covers it.

Cancellation relies on `kill_on_drop`: a late batch from a killed run must not
pollute the new one, so the delegate carries a generation counter and ignores
`SourceUpdate`s from a superseded run.

The `SourceRunner` seam is unchanged in shape — it still takes a command and
args and returns a stream — but the delegate now drives repeated spawns, and
`ScriptedRunner` must answer several `spawn` calls with different `args` to
test query-driven Finders.