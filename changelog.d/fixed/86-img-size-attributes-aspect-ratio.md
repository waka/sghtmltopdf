An `<img>` sized by its `width`/`height` attributes keeps its aspect ratio under
`max-width`/`max-height` (#86), so `img { max-width: 100%; height: auto }` no longer squashes
it. The attributes are now presentational hints, as in browsers: author CSS overrides them,
and `width="50%"` means 50% instead of 50px. An image with a set height and `width: auto` also
keeps its ratio under `max-height`.
