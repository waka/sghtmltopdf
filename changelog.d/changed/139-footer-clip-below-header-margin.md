Footer content (`--footer-html`) that reaches above the bottom margin is now drawn over the
content area instead of being clipped (#139). The footer is still laid out in the bottom margin,
but it is clipped to everything below the header's margin, never over the header's margin.
Normal-flow footer content never goes above the margin, so only what already reached above it
(a negative margin, a tall `position: absolute; bottom: 0` box, an upward `box-shadow` or
`outline`) newly shows. With `[page]`/`[topage]`, a footer can show a block such as signatures
above the margin on the last page only, while the body reserves that room only where it ends.
