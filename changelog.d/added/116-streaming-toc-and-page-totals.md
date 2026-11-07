Streaming mode now supports `--toc`, `counter(pages)` in `@page` margin boxes, and `[topage]`
in HTML headers and footers (#116). Decoration that needs the total page count is written at
the end as Form XObjects the body pages already refer to, so body pages are still written and
released as they settle. The table of contents is written last but ordered before the body.
`--page-offset` now applies to the page numbers in streaming mode as well.
