`@font-face` faces of one family are now chosen by weight as in CSS Fonts 4 (§ 5.2), not by
the order the rules are declared in (#151). `font-weight` keeps its number instead of being
reduced to normal or bold, so a 500 face is used for `font-weight: 500` even after a 400 one,
and a 300 face declared first no longer turns body text Light. A bold weight with no face of
600 or more is still drawn with synthetic bold. System fonts are still looked up as Regular or
Bold.
