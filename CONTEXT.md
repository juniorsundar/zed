# Context

Vocabulary for work done in this fork. Grown as needed — this is not a model of
Zed itself, only of the concepts added here and the upstream terms they are
most easily confused with.

## Language

### Finders

**Finder**:
A named, user-defined configuration that produces a searchable list of entries
and defines what happens when one is chosen.
_Avoid_: channel, custom picker, lens

**Source**:
The origin of a Finder's entries.

**Entry**:
A single selectable item produced by a Source. Always one whole line of the
Source's output — an Entry is never split into a shown part and a used part.

**Field**:
A slice of an Entry, named by its position when the Entry is divided on the
Finder's delimiter. An Outcome refers to Fields to act on part of an Entry
while the whole Entry stays visible.
_Avoid_: value, column, capture

**Query**:
The text in the Finder's search box. Its role depends on the Source. For a Source
that runs once, the Query is a client-side fuzzy filter applied to the Entries the
Source already produced. For a Source that re-runs per Query, the Query is an input
to the Source itself and the Source's output is the result set; the client does not
filter further.
_Avoid_: pattern, search term

**Preview**:
The secondary view showing detail about the currently selected Entry.

**Outcome**:
What happens when an Entry is chosen.
_Avoid_: action — in Zed an Action is a dispatchable named command, which an
Outcome may trigger but is not.

### Boundary terms

These belong to Zed, not to this fork. They are recorded only because the terms
above are easily confused with them.

**Picker**:
Zed's fuzzy-list widget. A Finder is a configuration that produces a Picker; it
is not itself one.

**Preview Tab**:
A Zed editor tab that the next opened file replaces, shown with an italic title.
Unrelated to a Preview, despite the shared word: a Preview is a pane inside a
Picker, a Preview Tab is a tab in the workspace. An Outcome that opens a file
decides whether it opens as a Preview Tab.

**Channel**:
A Zed collab channel. Unrelated to Finders, despite the term's use in similar
tools elsewhere.

**PathWithPosition**:
Zed's type for a path plus an optional 1-based row and column, as tools like
ripgrep print them. An Outcome that opens at a position produces one; the
Preview of such an Outcome shows the same one Confirm would open.
