Under `border-collapse: collapse`, borders set on `<tr>` and on `<thead>`/`<tbody>`/`<tfoot>` are
now drawn, merged with the cell borders by the same conflict rule, and take their width between
the rows as in browsers. A row with fewer cells than the table has columns still gets them across
the whole table width. Previously only cell borders were drawn, so these borders were missing.
