A table's head is no longer left alone at the bottom of a page, and `break-after: avoid` /
`break-before: avoid` on table rows are now honored (#138). When the head fit in what was left
of the page but the first body row did not, the head stayed alone at the foot of the page; now
the whole table starts on the next page. At a break inside a table, rows the page should not
break between move to the next page together, so a group header row is no longer separated
from the row below it.
