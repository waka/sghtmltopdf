A table row taller than a page now starts where it is and splits through the content of its cells (#163).
Before, the row was moved to a fresh page, leaving the page before it nearly empty, and whatever its cell held past one page was lost. This is the usual shape of an HTML email, whose whole body sits in one `<table><tr><td>`.
