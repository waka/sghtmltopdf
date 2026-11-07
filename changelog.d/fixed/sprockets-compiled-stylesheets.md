The Ruby `sghtmltopdf_stylesheet_link_tag` helper uses Sprockets' compiled CSS
instead of rereading the SCSS source, so Sass imports, variables and nesting work
without precompiling assets. Digested URLs in the compiled CSS are resolved back
to local assets in development too.
CSS imports are expanded even when compression removes the whitespace after
`@import`.
