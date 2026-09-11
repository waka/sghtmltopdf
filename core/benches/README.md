# Benchmark suite

Four `cargo bench` targets share one fixture corpus and one bundled font set
(`tests/fonts`). Every case also turns the engine's system font search off
(`--disable-system-fonts`), without which a family that has no matching face
in the bundled set — `font-family: serif` in italic, say — is filled in from
whatever the machine has installed, and the PDFs stop being comparable between
machines. With it, page counts and PDF sizes are identical on macOS aarch64
and Linux x86_64.

| Target | What it measures | Tool |
|---|---|---|
| `phases` | Each pipeline stage in isolation (parse, stylesheet, cascade, box tree, layout, paginate, encode) per fixture, plus one-off startup costs | criterion |
| `end_to_end` | The `Engine` API in batch and streaming mode, the CLI binary with its option variants (TOC, cover, header/footer, streaming, ...), and HTTP round trips against `sghtmltopdf server` | criterion |
| `scale` | Synthetic documents of 1k to 60k paragraphs and 1k to 40k table rows (the engine caps a document at 500k DOM nodes), batch and streaming, reported per element | criterion |
| `metrics` | Deterministic numbers with a baseline gate: allocation count and bytes, peak live heap, page count, PDF size, peak RSS, best-of-N time | custom harness |

## Running

```sh
cargo bench -p sghtmltopdf-core                 # everything (10 minutes or so)
cargo bench -p sghtmltopdf-core --bench phases  # one target
cargo bench -p sghtmltopdf-core --bench phases -- 'phase/flexbox/'   # regex filter
cargo bench -p sghtmltopdf-core --bench scale -- '/20000$'
cargo bench -p sghtmltopdf-core --bench end_to_end -- '^cli/'
```

Criterion keeps its history under `target/criterion` and prints the change
against the previous run of the same benchmark. To compare a branch against
`main` explicitly:

```sh
git switch main
cargo bench -p sghtmltopdf-core -- --save-baseline main
git switch my-branch
cargo bench -p sghtmltopdf-core -- --baseline main
```

HTML reports land in `target/criterion/report/index.html`.

## The deterministic gate (`metrics`)

Wall-clock time on a shared runner is noisy. The `metrics` target records the
numbers that are not, runs every case in a fresh child process, and compares
the result with `benches/baseline.json`:

```sh
cargo bench -p sghtmltopdf-core --bench metrics                    # compare, exit 1 on regression
cargo bench -p sghtmltopdf-core --bench metrics -- --save-baseline # rewrite the baseline
cargo bench -p sghtmltopdf-core --bench metrics -- --filter scale/ # subset (a filtered --save-baseline only replaces those cases)
cargo bench -p sghtmltopdf-core --bench metrics -- --json out.json # keep this run
cargo bench -p sghtmltopdf-core --bench metrics -- --markdown      # pipe table, for a PR comment
cargo bench -p sghtmltopdf-core --bench metrics -- --no-time       # time measured but not judged
cargo bench -p sghtmltopdf-core --bench metrics -- --runs 5 --list --no-fail
```

| Metric | Gate |
|---|---|
| Page count | fail on any change |
| PDF size | fail on any change |
| Allocation count, allocation bytes, peak live heap | fail above 5% growth |
| Peak RSS | fail above 10% growth, only when the baseline is from the same OS and architecture |
| Time (best of 3) | warn above 15%, fail above 30%; dropped by `--no-time` |

When a change is intentional (a layout fix that moves a page break, a smaller
font subset), re-run with `--save-baseline` and commit the new
`baseline.json` in the same PR so the diff documents the change.

CI (`.github/workflows/ci.yml`, job `bench`) runs this same gate against the
committed baseline with `--no-time`, since a shared runner's clock is not
comparable but its deterministic numbers are. The criterion targets run once
each (`--test`) there to catch a bench that stopped working.

## Corpus

`fixtures/*.html`, one document per feature area. Each is a few pages long so
every stage has measurable work. Two flags in `support/mod.rs` describe each
fixture: `streaming` (no `counter(pages)`, `<body>` decorations, or trailing
`<style>`, so it can run in streaming mode) and `local_assets` (reads images,
CSS, or fonts from disk, so it cannot be posted to the HTTP server).

| Fixture | Covers |
|---|---|
| `text_inline` | line breaking, inline styling, text-decoration, `white-space`, `text-overflow`, `pre`, links |
| `text_cjk` | Japanese shaping, mixed scripts, kinsoku, `text-emphasis` |
| `block_layout` | box model, logical properties, floats and clear, inline-block, absolute/fixed/relative, `box-shadow`, `border-radius`, `opacity`, `transform`, `aspect-ratio`, `overflow` |
| `flexbox` | direction, wrap, gap, grow/shrink/basis, justify/align, nesting |
| `grid` | templates, areas, spans, auto placement, `repeat()` |
| `table_basic` | collapsed and separate borders, colgroup, caption, rowspan/colspan, vertical-align, fixed layout, presentational attributes |
| `table_pagination` | a 400-row table across pages with repeating header, footer, `break-inside: avoid` rows |
| `paged_media` | `@page` size and margins, margin boxes, `counter(page)`, `break-*`, `orphans`/`widows` |
| `paged_media_total` | `counter(pages)`, `<html>`/`<body>` decorations, `:last-of-type` (batch only) |
| `generated_content` | `::before`/`::after`, `attr()`, `counter()`/`counters()`, quotes, every list-style type, `::first-letter` |
| `style_engine` | 800+ rules over 300 elements: level 4 selectors, `:has()`, nesting, custom properties, `calc()`, `color-mix()`, modern color spaces, attribute selectors, `@media` |
| `images` | PNG, JPEG, WebP, SVG file and data URI, `object-fit`, background images with repeat/position/size |
| `fonts` | `@font-face` with weights and `unicode-range`, family fallback, unknown families, wide glyph coverage, synthetic italic |
| `real_receipt` | the `examples/receipt.html` document |
| `real_invoice` | flex header, address blocks, 130-line table, totals, positioned stamp |
| `real_report` | 20 pages of headings, figures, tables, code, lists, internal and external links, `<base href>`; also the input for the CLI variants (TOC, cover, header/footer) |

## Investigating a regression

The benches tell you *that* something got slower and in which stage. To see
*why*, the examples under `core/examples` are the next step:
`phase_bench` prints the time and RSS of each stage for a synthetic document,
`heap_profile` attributes heap usage to call sites with `dhat`, and
`compare_engines` puts the numbers side by side with wkhtmltopdf and Chrome.
