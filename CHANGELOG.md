# CHANGELOG

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Hoist the rules inside `@layer` blocks to the top level, in source order, instead of
  dropping the whole block (#20). Tailwind v4 wraps its entire output in cascade layers,
  so a stock v4 bundle rendered a completely unstyled document. Layer precedence is not
  implemented: a print stylesheet is normally a single bundle with nothing to arbitrate
  against, so plain source order gives the same result, and where it differs the usual
  specificity contest decides. The bare `@layer a, b;` ordering statement is still ignored.

### Changed

- `--allow` is now spelled `--allow-path`, with `--allow` kept as an alias, so nothing
  has to change. On its own `--allow` says nothing about what it allows, and it sat next
  to `--allow-remote-assets`, which allows something else entirely. The Ruby key follows:
  `allow_path:`, with `allow:` normalized to it — the two are folded into one key rather
  than passed through as two, since repeating the flag means "add another directory", so
  a default under one spelling and a call-site value under the other would have been
  merged instead of replaced.

- The Rails defaults now let the engine read under `public/` and the asset pipeline load
  paths (`config.assets.paths`) rather than the whole of `Rails.root`. `config/`, `db/` and
  `storage/` are no longer reachable through an `<img src>` or a `url()` in a template,
  while the assets a gem or an engine provides — which live outside `Rails.root` and so
  were never covered — now are. An app that references a file elsewhere, say
  `Rails.root.join("tmp/chart.png")`, has to name that directory itself with
  `Sghtmltopdf.configure { |c| c.allow_path += ["…"] }`. The defaults are computed in
  `after_initialize` because the pipeline fills `config.assets.paths` in an initializer of
  its own, which runs after the one this gem adds.

### Fixed

- Read a `src` (or `url()`, or `href`) written as a filesystem path instead of joining it
  onto the base directory and looking for something that cannot be there. A reference
  starting with `/` is still resolved relative to the site root first, which is what the
  Rails asset pipeline emits and what every document that works today relies on; only when
  no file is there is the same string read again as an absolute path. Whether it may be
  read is decided by the existing rules, so one inside the base directory is read as it is
  and one outside it needs `--allow-path`. `<img src="/var/www/app/public/logo.png">` used to
  look for `<base directory>/var/www/app/public/logo.png` and could not be made to work by
  any flag; when neither reading finds a file, the error now names both paths.
- `sghtmltopdf_image_tag` no longer hands a filesystem path to `image_tag` (#44). Rails
  turned that path into a URL — with `default_url_options[:host]` set, an `http://` one the
  engine refused to fetch, and without it an absolute path that was resolved against
  `base_url` and missed — so the helper documented for local images could not load one. It
  now looks the file up in the asset pipeline and references it by path: relative to
  `base_url` when it sits under it, and the absolute filesystem path otherwise, which the
  engine reads as a filesystem path once it fails to resolve under `base_url`. A file that
  `allow_path` does not cover, or a run delegated to a server that may not share this
  filesystem, is embedded as a `data:` URI instead, so a path the engine cannot read
  cannot silently vanish from the PDF; `inline: true` embeds unconditionally. The `size:`
  shorthand is expanded into `width`/`height`, as `image_tag` does. `sghtmltopdf_asset_path`
  also stops mapping a source that is already a URL onto a same-named file under `public/`,
  and no longer gives up on a `public/`-only file in a Propshaft app, where `asset_path`
  raises `MissingAssetError` rather than returning a path.
- `sghtmltopdf_stylesheet_link_tag` now points the `url()`s of the CSS it inlines at files
  the engine can read, instead of copying the file verbatim (#45). The asset pipeline
  rewrites every `url()` through `asset_path` while precompiling, so a `@font-face` source
  became a digested `/assets/…` path, or an absolute `https://…` URL once `asset_host` was
  set; rendering never goes through the HTTP server, so neither could be fetched and the
  family fell back to the engine default rather than to the next `font-family` — silently,
  since a `@font-face` that cannot be loaded only warns. Each reference is now mapped back
  onto its file — the path part of an absolute URL, a site-root-relative path under
  `public/` or the pipeline load path, a relative one against the stylesheet's own
  directory as CSS says it means — and written the way `sghtmltopdf_image_tag` writes an
  image: relative to `base_url`, the absolute path when it sits elsewhere the engine may
  read, a `data:` URI when it may not. A reference that names no file of the application,
  such as a font served by a CDN, is left alone. `@import` is spliced in rather than left
  for the engine, which resolves every `url()` against the document's base whatever
  stylesheet it came from, so the same problem would reappear one level down.
- `position: relative` now moves the content of the element together with its background
  and border (#29). The offset was applied to the box's own rectangle after its lines and
  child boxes had been placed, so text, images, nested blocks and list markers were left
  at the unoffset position. A `position: relative` inline element (`<span>`) now shifts
  its own text too, and an absolutely positioned descendant of a relative element uses the
  offset padding box as its containing block.
- Keep the spacing that margin collapsing produced when a document is split across pages.
  The pagination rebuilt each page by stacking margin boxes at a running cursor, which
  reopened every margin the layout had collapsed: adjacent siblings were pushed apart by
  the smaller of the two margins (paragraphs 50.6px apart instead of 34.7px with the
  default stylesheet), and a `margin-top` hoisted out of a first child was added once per
  ancestor, so the top of the first page was pushed down by a multiple of it (85.8px
  instead of 21.4px for `<h1>` under `<html><body>`). Boxes are now placed at the offsets
  the layout gave them, so a document that spans several pages has the same geometry as
  the same content on a single page, and pages hold as much as they should.
- Start a `display: grid` or `display: table` on the next page when its first row does not
  fit in what is left of the current one. The row-splitting rule only broke once a fragment
  already held a row, so the first row was laid down at the bottom of the page whatever the
  space left and was cut off by the page edge, where blocks and flex containers move on.
- Place the row bands of a `display: grid` container the same way as everything else when
  its subtree is moved vertically. `shift_box_y_in_place` and `shift_content_vertical`
  added the delta to `LaidOutGridRow`'s `top`/`bottom` while subtracting it from every
  other coordinate, so a paginated grid under collapsing margins started its second page
  above the top of the page and lost the rows there.
- Paint the rows of a `display: grid` container on the pages it was split across (#18).
  The pagination allocated the right number of pages and moved each row band into page
  coordinates, but shifted the items inside the band the opposite way, so every page after
  the first came out blank and the paragraphs on them were missing from the PDF.
- Split a `display: flex` container that is taller than a page instead of silently losing
  everything past the first page (#18). A flex container that fits on a page is still
  moved to the next page whole; one that cannot fit anywhere is now split between bands
  of items that do not overlap vertically (each item of a column flex, each line of a
  wrapped row flex), and a band holding a single item is split inside like a block, so a
  long document body laid out with `flex-direction: column` flows across pages. The space
  `gap` (or `justify-content`) leaves between the bands is carried across the split, so a
  container that grows past one page keeps the spacing it had.
- `text-align` now moves inline images and `inline-block` boxes along with the text (#19).
  A line box keeps its text runs and its atomic inline boxes (`<img>`, `display: inline-block`,
  form controls) in separate lists, and the alignment step only shifted the runs, so a
  right-aligned header rendered its title on the right and its logo flush left. `justify`
  counts the space before a box as a justification opportunity and shifts the box together
  with the words around it, rather than pushing the words into it.
- `text-align` is now read from the block container that establishes the inline formatting
  context, rather than from the first text span in it. `text-align` applies to block
  containers and is only inherited by inline boxes, so a value set on an inline one was
  never meant to win: `<div style="text-align: right"><span style="text-align: left">WORD</span></div>`
  left the word at x=0. A line holding nothing but boxes (`<div style="text-align: right"><img></div>`)
  had no text span to read at all and was therefore always aligned left.

## 0.3.0 - 2026-08-30

### Added

- Map the logical box properties to their physical sides (#21). `margin-inline-start`
  becomes `margin-left`, `padding-block` becomes `padding-top` and `padding-bottom`, and so
  on for `margin-*`, `padding-*`, `inset-*` and `border-*` (including the `-width`, `-style`
  and `-color` longhands), plus the `inset` shorthand and the logical corner radii
  (`border-start-start-radius` and its three siblings). The engine only supports
  `horizontal-tb` LTR, so the mapping is fixed rather than driven by `writing-mode`.
  Tailwind v4 emits these for `px-*`, `py-*`, `mx-auto` and `space-y-*`, and until now a
  document built with it silently lost its horizontal padding and every centred block.

- Render SVG referenced from `<img src>` and `background-image: url()`, as vector graphics
  rather than by rasterising. Parsing goes through [usvg] and the translation to PDF
  drawing operators through [svg2pdf], both from typst. An SVG becomes a form XObject
  normalised to the unit square, so the existing drawing, `object-fit`, background tiling
  and per-`src` caching all apply unchanged. `.svgz` (gzipped) is accepted too.
  Behind the `svg` feature, on by default.
- The `svg-text` feature (off by default) renders `<text>` inside an SVG as embedded,
  selectable glyphs, using **the document's own fonts**. Whatever is available to the HTML
  is available inside the SVG, resolved the same way: by the font's internal family name, by
  the name it is declared under in CSS (`@font-face`), or through the generic families —
  `serif` / `sans-serif` / `monospace` in an SVG land on `--serif-font` / `--gothic-font` /
  `--mono-font`. Text with no `font-family`, and text naming a family the document does not
  have, both fall back to the document's default font. No separate system-font scan happens
  for SVG, so an SVG can never come out in a font the document never had.

  It is off by default because enabling it adds 25 crates (rustybuzz, resvg and friends,
  pulled in by svg2pdf's `text` feature). Without it, text inside an SVG is **not drawn at
  all** — not even converted to paths, since svg2pdf discards text nodes outright. That is
  now reported: an SVG containing `<text>` warns once per document. usvg and svg2pdf do log
  it themselves, but through the `log` crate, and this crate installs no logger, so it never
  reached anyone.

- Accept non-base64 `data:` URIs. A payload without `;base64` is now percent-decoded per
  RFC 2397 instead of rejected. This is how SVG data URIs are normally written
  (`data:image/svg+xml,%3Csvg...%3E`), in `<img src>` and CSS `url()` alike; requiring
  base64 made the common form fail. Tabs and newlines are dropped from the payload (as the
  URL standard does) but spaces are kept, since they separate tokens in an unencoded SVG.

### Changed

- Pinned `pdf-writer` to 0.12 so that svg2pdf's `Chunk` is the same type as the one used to
  write the document, which is what lets an SVG be spliced in without going through bytes.
  No API of ours changed as a result.
- `PreparedImage`'s intrinsic size is now `f32` rather than `u32`. An SVG's intrinsic size
  can be fractional (`width="40.6"`, a fractional `viewBox`), and rounding it changed the
  aspect ratio — 40.6×10.4 became 41×10, a 5% error that visibly skewed `object-fit`
  (`contain` gave a height of 24.4 instead of 25.6) and the height derived from a
  `width`-only rule. Raster sizes are unaffected: they are whole pixels either way.
- An inline `<svg>` in the HTML now warns once per document instead of silently rendering
  nothing. It is still not drawn — only `<img>` and `background-image` references are — but
  saying "SVG is supported" and then dropping inline SVG without a word was misleading.
- `line-height: normal` is now the font's own recommended line spacing (ascent + descent +
  line gap), as CSS defines it, rather than a fixed 1.2em (#33). The fixed ratio only worked
  for fonts whose content area fits inside it: DejaVu Sans needs 1.164em and Liberation Sans
  1.150em, but Noto Sans CJK needs 1.448em, and CJK fonts around 1.4em are common. Where the
  ratio was too small the half-leading went negative and the glyphs spilled out of their line
  box, so the last line of a block overlapped whatever came next — a table cell's
  `border-bottom` drawn through the text, for instance. The overflow scales with `font-size`,
  which is why it appeared when a larger font followed a smaller one and stayed invisible in
  the other order.

  Line spacing therefore changes in any document that leaves `line-height` unset. Latin text
  tightens slightly (1.2em to 1.164em with DejaVu Sans); Japanese text loosens by roughly a
  fifth (1.2em to 1.448em with Noto Sans CJK), so some documents will gain pages. A document
  that sets `line-height` explicitly, as a number or a length, is unaffected and its output is
  unchanged byte for byte. An explicit value smaller than the font's content area still
  overflows its line box, exactly as it does in a browser; that is the specified behaviour and
  is deliberately left alone.

- Write a file identifier (`/ID`) into the PDF trailer. PDF/A requires one, and tooling that
  tracks a file across revisions expects it. The value is 16 bytes of a hash over the same
  metadata, creation date and page count that go into the `/Info` dictionary, so it is stable
  for a given document; batch and streaming output produce it the same way. No incremental
  update is ever written, so the two array elements are equal.
- Reject a `calc()` or a parenthesised group nested deeper than 32 levels, dropping the
  declaration as an invalid value. The value parser is recursive descent, so nesting depth is
  stack depth, and untrusted CSS could overflow the stack (even the 16 MB rendering stack goes
  at around twenty thousand levels). Real stylesheets stay within a handful of levels.

### Fixed

- Parse nested style rules (CSS Nesting) instead of silently dropping them (#25).
  `.wrap { & .probe { } }`, `.wrap { .probe { } }`, `.wrap { &.probe { } }` and
  `.list { > li { } }` now reach the cascade with the meaning the spec gives them; `&`
  takes the parent's specificity, and declarations written after a nested rule keep
  their source position instead of being hoisted above it. Nested at-rules such as
  `@media` inside a style rule are still ignored.
- Accept `calc()` as a term inside another `calc()` (#17). CSS Values 4 treats a nested
  `calc()` the same as a parenthesised group, but the parser only handled the parentheses,
  so `calc(calc(45px * 2) * calc(1 - 0))` was rejected as invalid and the declaration was
  dropped while `calc((45px * 2) * (1 - 0))` resolved to 90px. Tailwind v4 emits the nested
  form for every `space-y-*` and `divide-*` utility, so a Tailwind bundle lost all of its
  vertical rhythm and divider gaps.
- Stop rounding flex and grid item sizes to whole pixels (#15). taffy rounds its final
  layout to integers so that a rasteriser does not leave gaps or overlaps between boxes;
  the output here is PDF, which has no such constraint, and the rounding truncated the
  measured max-content width so that text which fit was wrapped onto a second line. Which
  way the fraction rounded depended on the exact string, so the same row wrapped for one
  value and not for another (`1 USD = 0.9143 EUR` wrapped, `1 USD 0.9143 EUR` did not).
- Write the required `/CMapName` and `/CIDSystemInfo` entries into the `/ToUnicode`
  CMap stream dictionary. ISO 32000-1 table 120 lists both as required for a CMap
  stream dictionary; the values were already declared inside the embedded CMap
  program but not lifted into the dictionary. Strict PDF tooling (e.g. HexaPDF,
  veraPDF) rejects the file without them, which blocks PDF/A-3 validation and
  therefore Factur-X / ZUGFeRD hybrid e-invoice embedding.
- Keep the outside marker on a list item that is split across pages (#31). An item that did
  not fit in what was left of a page kept its marker gutter but lost the marker itself, so an
  ordered list that paginated silently skipped numbers (7., 14. and 21. in the reported
  document); the numbers were missing from the text layer too, not merely clipped. Pagination
  moves the marker onto the item's first fragment, but fragments were only produced for a
  container that actually paints a background or border. A plain `li` paints neither, so the
  marker was taken off the container with nowhere to put it back. On a decorated item the
  marker survived but carried its pre-pagination coordinates, which placed it at a position
  belonging to another page; that is corrected as well.
- Count a table's columns with the occupancy of `rowspan` taken into account (#32). The column
  count was the largest per-row sum of `colspan`, while cell placement skips the columns still
  held by a `rowspan` from an earlier row. When the first row held nothing but a `rowspan="2"`
  cell, both rows summed to one column, so the second row's cell was placed in a column the
  table did not have, given zero width, and dropped from the output with no error or warning —
  a logo beside a label that appears on the following row, a common invoice-table shape. The
  count now falls out of the placement walk itself, so the two can no longer disagree.
- Generate the anonymous table boxes that CSS 2.1 §17.2.1 calls for (#34). Content inside a
  `display: table` was laid out only when every cell sat inside an explicit `display: table-row`
  box; a stray `table-cell`, or a plain block child, was dropped from the output with no text,
  no ink and no warning. Consecutive cells without a row now get an anonymous row (rule 2.1),
  and consecutive children of a table or a row that are not cells get an anonymous cell
  (rule 2.2). Whitespace between proper table children, and `<colgroup>` / `<col>`, still
  generate nothing. `display: table` with `display: table-cell` and no row in between is a
  common pre-flexbox column idiom, so a document using it lost whole columns silently.
- Drop a negative `padding` declaration instead of honouring it. CSS defines a padding of less
  than zero as invalid, and a negative value shrank the content box in a way no browser
  reproduces. A `calc()` is still accepted, since its sign is not known until it is resolved.

### Known limitations

- SVG filters (`<filter>`) and raster images inside an SVG (`<image>`) are not drawn.
  Filters would require rasterising, which this deliberately avoids.
- Inline `<svg>` written directly in the HTML is not rendered; reference the SVG from
  `<img>` or `background-image` instead. Supporting it means rebuilding SVG XML out of the
  HTML DOM and deciding how attribute case (`viewBox`), CSS inheritance and `currentColor`
  carry across — a different problem from referencing a file, so this phase covers only
  references.
- `--grayscale` does not apply to SVG. It warns and leaves the SVG in colour.
- External references from inside an SVG (`<image href="...">`) are refused with a warning
  rather than resolved. usvg's default resolver reads such an href straight off disk, which
  would bypass the containment that applies to `<img>` (base directory, `--allow`,
  `--disable-local-file-access`), so the path is closed off entirely. `data:` URIs are
  unaffected, being self-contained.

[usvg]: https://github.com/linebender/resvg
[svg2pdf]: https://github.com/typst/svg2pdf

## 0.2.0 - 2026-08-16

### Added

- Support the `:has()`, `:is()` and `:where()` selectors (#10). Specificity follows the
  spec: `:is()` and `:has()` count as their most specific argument and `:where()` counts as
  zero, and the argument list of `:is()` / `:where()` is forgiving. In streaming mode
  `:has(~ ...)` cannot be decided and warns.
- Support `color-mix()` (#11), in the `srgb`, `srgb-linear`, `lab`, `oklab`, `xyz`, `hsl`,
  `hwb`, `lch` and `oklch` colour spaces with all four hue interpolation methods. Weight
  normalisation and premultiplied alpha follow the spec. Wide-gamut spaces and
  `currentcolor` operands are rejected; see the docs for why.
- Accept `data:` URIs and `http(s)` URLs in the `src: url()` of `@font-face` (#5). They are
  resolved through the same fetcher, `<base href>` handling and access control as `<img>`,
  `<link>` and `@import`.
- Support `<wbr>`, and U+200B ZERO WIDTH SPACE, as a line break opportunity. Neither
  adds width nor leaves a character in the PDF text layer.

### Changed

- Decline a font that has no glyph outlines, with a warning naming the font, instead of
  selecting it (#9). Colour emoji fonts such as Noto Color Emoji are bitmap-only: font
  selection consulted `cmap` alone, so such a font was chosen as one that could draw the
  character, and the result was text that vanished entirely rather than showing tofu, with
  no warning, a PDF inflated to the size of the source font because subsetting had nothing
  to strip, and an embedded font some readers refused to parse. Emoji now fall back to tofu
  with a warning naming the characters. Colour emoji rendering itself is tracked in #12; a
  monochrome outline font such as Noto Emoji works today through `--font`.
- Report only the selectors that actually behave differently in streaming mode. The warning
  used to name `:last-child` and `:empty`, which are correct there, while staying silent
  about `+`, `~` and `:first-child`, which were not.

### Fixed

- Measure the natural width of a nested table, flex or grid box instead of treating it as
  zero (#5). A grid or flex container nested inside another one collapsed to zero width,
  so its content overflowed one word per line. This was never specific to grid-in-grid:
  flex-in-flex, flex-in-grid, grid-in-flex and any of those inside a table cell took the
  same path.
- Let `auto` grid tracks absorb the leftover width. `justify-content` had `flex-start` as
  its initial value internally, which is not the same as the initial `normal` and stopped
  the tracks from stretching.
- Collect absolutely positioned descendants of a flex item, a grid item, a table cell and
  an `inline-block` (#5). They were laid out through helpers that discarded them, so the
  element was silently dropped.
- Keep the preceding siblings of a processed top-level element visible in streaming mode.
  The subtree was released as soon as it had been laid out, so every later element looked
  like the first child: `+` and `~` stopped matching and `:first-child` matched everything.
  The nodes are now kept when the stylesheet needs them, which costs about 19 bytes per
  top-level element and nothing at all otherwise.
- Memoise the natural width and the measured height of each box. Deeply nested flex and
  grid re-measured the same subtree once per ancestor level, growing exponentially with
  depth; a five-level structure repeated 200 times went from 0.15 s to 0.04 s.
- Keep the whitespace between two inline elements, so `<span>one</span> <span>two</span>`
  renders as `one two` instead of `onetwo` (#3).
- Collapse only the whitespace CSS Text 3 says is collapsible (space, tab, newline).
  `&nbsp;` and the other Unicode spaces are no longer collapsed into a single space
  and keep their own advance width, so `&nbsp;&nbsp;&nbsp;` is three spaces wide and
  thin/hair/em spaces are no longer all rendered as one plain space.
- Do not wrap around `&nbsp;`, narrow no-break space, figure space or word joiner
  (UAX #14 glue), including under `word-break: break-all`. Thin space and friends
  offer a wrap opportunity after them, and U+200B ZERO WIDTH SPACE now provides a
  zero-width break opportunity inside a word.
- Treat a cell holding only `&nbsp;` as non-empty, so `empty-cells: hide` no longer
  strips the borders of a `<td>&nbsp;</td>`.
- Do not let a glyph shared by several characters lose its `/ToUnicode` mapping to
  whichever character happened to come first in the document. A font without its own
  `&nbsp;` glyph made every space in the document extract as U+00A0, breaking
  copy-paste and text search in the PDF.
- Draw glyphs at the advance width the layout used. A PDF advances a glyph by its
  single `/W` entry, which cannot express the two cases where the shaper reports a
  different advance for the same glyph: the stretched word gaps of a justified line,
  and a fixed-width space (`&thinsp;` and friends) that the font has no glyph for.
  A `text-align: justify` line was drawn short of the right edge by the whole stretch
  amount, and text following a substituted fixed-width space was drawn off its laid
  out position. The difference is now made up with `TJ` adjustments.
- Keep the leading whitespace of a `white-space: pre` element when it comes from a
  whitespace-only text node, so `<pre>   <b>x</b>y</pre>` keeps its indentation
  instead of rendering as `xy`.

## 0.1.1

### Changed

- Change gemspec from Japanese to English.
- Change required Ruby version for precompiled gem.

## 0.1.0

### Added

- 1st release on 2026-08-08
