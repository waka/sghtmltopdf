`--tagged` can now be combined with `--streaming` (#132). The structure tree is
collected one top-level element at a time, before that element's subtree is
freed, and written when the document is finished, so streaming output carries
the same headings, paragraphs, lists, tables, links and figures as batch mode.
The cover and TOC pages, which are separate documents, stay untagged. Tagged
streaming retains the structure information (one entry per tagged element and
marked-content sequence) until the end of the document, so its memory use
grows with the size of the structure tree even though page content is still
written out as each page settles.
