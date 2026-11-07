`::before` and `::after` are boxes of their own (#152). Declarations on the pseudo-element
now apply to its generated content: `font-size`, `color`, `font-family`, `display`, `margin`,
`padding`, `background`, `position`, `transform` and the rest, with anything not set inherited
from the element, as for a first or last child. Before, the content was drawn with the
element's own style, glued to its text. They are also generated on an element with block
children, which used to get none, and `display: none` on them generates nothing.
