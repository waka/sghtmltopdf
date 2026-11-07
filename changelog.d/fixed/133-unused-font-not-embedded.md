A font that draws no text is no longer embedded in the PDF or listed in its page resources (#133).
In `--streaming` mode with no `--font` or `@font-face`, the CJK fallback font that is loaded up
front for CJK text arriving later in the stream was embedded even when the document had none, and
a document without text embedded the default font. Each page now lists only the fonts it draws
with. The same applies without `--streaming`: a `--font` that draws nothing (DejaVu Sans on a page
of only Japanese text, say) is no longer embedded either.
