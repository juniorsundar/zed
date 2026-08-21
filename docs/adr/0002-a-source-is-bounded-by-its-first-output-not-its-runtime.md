# 2. A Source is bounded by its first output, not its runtime

## Status

Accepted

## Context

Sources originally ran to completion before any Entry was shown, so a single
timeout over the whole run was both meaningful and safe: five seconds was the
longest a user should wait for a list to appear.

Making Sources stream broke that. A Source that walks a large tree may
legitimately take a minute to finish while producing usable Entries throughout,
and under a whole-run timeout it would be killed mid-flight — with the Entries
already on screen replaced by a timeout message. The same applies to the output
cap: a byte limit cannot be applied to something that has no end, and the
existing limit reported "this source needs streaming", which is no longer a
thing a user can act on.

## Decision

The timeout guards a Source's *first* output only. If nothing at all arrives
within `SOURCE_TIMEOUT`, the Source is abandoned. Once anything has arrived, the
Source is never abandoned for slowness, however long the gaps between Entries.

The byte cap is replaced by an Entry cap, `MAX_ENTRIES`. Reaching it is not a
failure: the Source is dropped, the Entries that arrived are kept, and the
Finder reports that the list is truncated.

## Consequences

A Source that produces one line and then runs forever is never killed while its
Finder is open. This is deliberate — it is indistinguishable from a slow but
correct Source, and the user can see Entries and dismiss the Finder, which drops
the stream and kills the child. The bound that remains is `MAX_ENTRIES`, which
catches the runaway case that actually costs memory.

A Source that fails *after* producing Entries became reachable, where before a
non-zero exit meant no Entries at all. Those Entries are kept and the failure is
reported alongside them rather than replacing them, so no error is silently
dropped.

The `SourceRunner` seam changed shape to carry this: it hands back a stream of
line-and-exit events instead of a completed `Output`. Exit status and stderr
arrive as the final event, which is what allows a failure to be reported after
Entries without discarding them.
