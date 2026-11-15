An opaque background on a parent no longer hides a float that continues onto the next page
(#159). The parent's background and borders on that page were drawn after the float and
covered it, leaving its text in the PDF but invisible. They are now drawn underneath.
