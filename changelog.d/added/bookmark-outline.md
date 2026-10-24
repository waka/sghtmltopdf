The PDF gets a document outline (bookmarks) from the CSS `bookmark-level`, `bookmark-label`
and `bookmark-state` properties. An element with a `bookmark-level` becomes an entry,
nested by level in document order, labelled with its `bookmark-label` (strings, `attr()` and
`content()`; the element's text by default) and jumping to where the element starts. A
document without such elements gets no `/Outlines`. It works in both batch and streaming mode.
