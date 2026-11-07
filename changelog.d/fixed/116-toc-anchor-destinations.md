With `--toc` or `--cover`, internal links to headings and other anchors could point at a page
of the table of contents or the cover instead of the body (#116). Those pages are separate
documents whose element IDs collided with the body's anchors. In streaming mode, anchors in
the part of the document read after the first top-level element now get link destinations too.
