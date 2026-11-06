`border-radius` now clips images, not just the background colour (#150). An `<img>` is
trimmed to its content edge curve (the radius less its border and padding), a box with
`overflow: hidden` clips its children to its padding edge curve, and a `background-image`
is clipped to the border edge curve, as CSS Backgrounds 3 § 5.3 asks. A round avatar
(`rounded-full border-2`) used to print as a square whose corners covered the border ring.
